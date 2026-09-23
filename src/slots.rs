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
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{Error, Result};

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
    /// `CARGO_BUILD_JOBS` value dispatch injects (default 4).
    pub jobs_per_lane: usize,
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
}

impl Default for SlotConfig {
    fn default() -> Self {
        Self {
            build_slots: 3,
            suite_slots: 1,
            jobs_per_lane: 4,
            starve_secs: 900,
            priority_lanes: Vec::new(),
            max_hold_secs: 7200,
        }
    }
}

/// Slot kinds map to pools: `build`/`test` share `build_slots`,
/// `suite` owns `suite_slots`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SlotKind {
    Build,
    Test,
    Suite,
}

impl SlotKind {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "build" => Ok(Self::Build),
            "test" => Ok(Self::Test),
            "suite" => Ok(Self::Suite),
            _ => Err(Error::rejected(format!(
                "Slot kind must be build, test or suite — got '{name}'"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Test => "test",
            Self::Suite => "suite",
        }
    }

    /// The pool this kind draws on: suite is independent, build and
    /// test share — `kind` still rides the queue for reporting.
    fn pool(self) -> Pool {
        match self {
            Self::Suite => Pool::Suite,
            _ => Pool::Build,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pool {
    Build,
    Suite,
}

impl Pool {
    fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Suite => "suite",
        }
    }
}

/// The pool a hold blocks queueing into — `holds_other_pool`'s name.
fn other_pool(pool: Pool) -> Pool {
    match pool {
        Pool::Build => Pool::Suite,
        Pool::Suite => Pool::Build,
    }
}

struct SlotWait {
    request_id: String,
    kind: SlotKind,
    lane: String,
    pid: u32,
    pid_start: Option<u64>,
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

    /// Where holds persist — the daemon points this at
    /// `<state>/slots.json`; unit tests leave it unset.
    pub fn persist_to(&mut self, path: PathBuf) {
        self.persist_path = Some(path);
    }

    fn capacity(&self, pool: Pool) -> usize {
        match pool {
            Pool::Build => self.config.build_slots.max(1),
            Pool::Suite => self.config.suite_slots.max(1),
        }
    }

    fn held_in(&self, pool: Pool) -> usize {
        self.held.iter().filter(|h| h.kind.pool() == pool).count()
    }

    /// Grant-order rank: starved `(lane, kind)`s first, then priority
    /// lanes on test/suite, then plain FIFO — the key is
    /// (rank, queued_at). Stamps were clamped at ENQUEUE time
    /// (`stamp_for`), not here — clamping at compare time would
    /// collapse every starved waiter to the same key and let a
    /// 901-second waiter tie a two-hour one; raw stamps keep rank 0
    /// in true FIFO order.
    fn rank(&self, w: &SlotWait, now: f64) -> (u8, f64) {
        if now - w.queued_at >= self.config.starve_secs as f64 {
            (0, w.queued_at)
        } else if matches!(w.kind, SlotKind::Test | SlotKind::Suite)
            && self.config.priority_lanes.iter().any(|l| l == &w.lane)
        {
            (1, w.queued_at)
        } else {
            (2, w.queued_at)
        }
    }

    /// Ranks for the whole queue — computed once per call so position
    /// and is_next are O(n) scans over O(n) rank evaluations.
    fn ranks(&self, now: f64) -> Vec<(u8, f64)> {
        self.waiting.iter().map(|w| self.rank(w, now)).collect()
    }

    /// How many same-pool waiters outrank entry `idx`.
    fn outranked(&self, ranks: &[(u8, f64)], idx: usize) -> usize {
        let pool = self.waiting[idx].kind.pool();
        ranks
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != idx && self.waiting[*i].kind.pool() == pool)
            .filter(|(_, r)| **r < ranks[idx])
            .count()
    }

    /// Drop dead/recycled/expired holders and dead or silent waiters.
    /// Holder drops emit `slot_released` with the reap reason (and no
    /// token — events never carry one); a dead waiter held nothing,
    /// so it drops silently. Returns what was reaped so `release`
    /// can answer a just-reaped token softly. Strict holds follow the
    /// tri-state rule instead ([`Self::reap_strict`]), and strict
    /// waiters are revalidated against their enrollment here — before
    /// any caller ranks the queue.
    fn reap_dead(&mut self, now: f64, events: &mut Vec<SlotEvent>) -> Vec<Reaped> {
        let mut dead: Vec<Reaped> = Vec::new();
        self.held.retain(|h| {
            if h.strict.is_some() {
                return true;
            }
            let gone = !pid_matches(h.pid, h.pid_start);
            let expired = now - h.acquired_at >= self.config.max_hold_secs as f64;
            if !gone && !expired {
                return true;
            }
            // `pid_alive` distinguishes a recycled pid (alive, wrong
            // starttime) from a dead one in the reason.
            let reason = if expired && !gone {
                "hold expired"
            } else if pid_alive(h.pid) {
                "pid recycled"
            } else {
                "holder died"
            };
            events.push((
                h.lane.clone(),
                "slot_released",
                json!({"kind": h.kind.as_str(), "pid": h.pid,
                       "held_secs": (now - h.acquired_at).max(0.0),
                       "reason": reason}),
            ));
            dead.push(Reaped {
                token: h.token.clone(),
                lane: h.lane.clone(),
                kind: h.kind,
                reason,
            });
            false
        });
        if !dead.is_empty() {
            self.persist();
        }
        self.expire_enrollments(now, events);
        self.reap_strict(now, events, &mut dead);
        // Waiters drop on either abandonment signal: a dead/recycled
        // pid, or a poll gone silent past the TTL (a fast-failed
        // --wait-secs 0 caller's pid may still be alive in its parent
        // — only the silence proves it walked away). A strict waiter
        // also drops — reported — once its enrollment is revoked,
        // expired, gone or generation-drifted, or its holder is not
        // provably alive: the strict check never uses the alive-only
        // fallback.
        let (proc, enrollments) = (&self.proc, &self.enrollments);
        self.waiting.retain(|w| {
            let Some(b) = &w.strict else {
                return pid_matches(w.pid, w.pid_start) && now - w.last_poll <= WAITER_TTL_SECS;
            };
            let why = match enrollments.iter().find(|e| e.id == b.enrollment_id) {
                None => Some("enrollment gone"),
                Some(e) if e.auth != AuthState::Active => Some(e.auth.as_str()),
                Some(e) if e.owner_generation != b.owner_generation => {
                    Some("owner generation changed")
                }
                Some(_) => match proc.liveness(&b.holder).0 {
                    Liveness::Alive => None,
                    Liveness::Dead => Some("waiter died"),
                    Liveness::Unknown => Some("waiter liveness unknown"),
                },
            };
            if let Some(why) = why {
                events.push((
                    w.lane.clone(),
                    "slot_wait_dropped",
                    json!({"kind": w.kind.as_str(), "pid": w.pid,
                           "request_id": w.request_id, "reason": why}),
                ));
                return false;
            }
            now - w.last_poll <= WAITER_TTL_SECS
        });
        self.prune_seniority(now);
        dead
    }

    /// Strict holds: tri-state liveness of the exact recorded holder.
    /// `dead` frees — through the fail-closed writer, so a failed
    /// write keeps the hold accounted — while `alive` and `unknown`
    /// retain. Revocation, expiry and `max_hold_secs` never free a
    /// strict hold (see [`Self::accounting`]).
    fn reap_strict(&mut self, now: f64, events: &mut Vec<SlotEvent>, dead: &mut Vec<Reaped>) {
        let mut gone: Vec<(String, &'static str)> = Vec::new();
        for h in self.held.iter_mut() {
            if let Some(b) = &mut h.strict {
                let (liveness, why) = self.proc.liveness(&b.holder);
                b.liveness = liveness;
                if liveness == Liveness::Dead {
                    gone.push((h.token.clone(), why));
                }
            }
        }
        if gone.is_empty() {
            return;
        }
        let next: Vec<SlotHold> = self
            .held
            .iter()
            .filter(|h| !gone.iter().any(|(t, _)| *t == h.token))
            .cloned()
            .collect();
        let freed: Vec<SlotHold> = self
            .held
            .iter()
            .filter(|h| gone.iter().any(|(t, _)| *t == h.token))
            .cloned()
            .collect();
        if self.commit(self.enrollments.clone(), next).is_err() {
            return;
        }
        for h in freed {
            let reason = gone
                .iter()
                .find(|(t, _)| *t == h.token)
                .map(|(_, why)| *why)
                .unwrap_or("holder died");
            events.push((
                h.lane.clone(),
                "slot_released",
                json!({"kind": h.kind.as_str(), "pid": h.pid,
                       "held_secs": (now - h.acquired_at).max(0.0),
                       "reason": reason}),
            ));
            dead.push(Reaped {
                token: h.token,
                lane: h.lane,
                kind: h.kind,
                reason,
            });
        }
    }

    /// Watcher pass (CAD-230b): free every strict hold whose exact
    /// holder is proven dead — no client call needed. `alive` and
    /// `unknown` retain, a failed write keeps the hold accounted
    /// (see [`Self::reap_strict`]). Legacy holds keep their CAD-113
    /// reap-on-call rule untouched. Freed tokens are remembered
    /// (bounded) so their holder's `release` still answers softly.
    pub fn reap_strict_holds(&mut self, now: f64) -> Vec<SlotEvent> {
        let (mut events, mut dead) = (Vec::new(), Vec::new());
        self.reap_strict(now, &mut events, &mut dead);
        self.recent_reaped.extend(dead);
        let excess = self.recent_reaped.len().saturating_sub(RECENT_REAPED);
        self.recent_reaped.drain(..excess);
        events
    }

    /// Active enrollments past their daemon-capped lifetime become
    /// `expired`: no new work, holds untouched.
    fn expire_enrollments(&mut self, now: f64, events: &mut Vec<SlotEvent>) {
        let due: Vec<usize> = (0..self.enrollments.len())
            .filter(|i| {
                let e = &self.enrollments[*i];
                e.auth == AuthState::Active && now >= e.expires_at
            })
            .collect();
        if due.is_empty() {
            return;
        }
        let mut next = self.enrollments.clone();
        let mut expired = Vec::new();
        for i in due {
            next[i].auth = AuthState::Expired;
            let e = &next[i];
            expired.push((
                e.owner_actor.clone(),
                "slot_enrollment_expired",
                json!({"enrollment_id": e.id, "root_pid": e.root.pid}),
            ));
        }
        if self.commit(next, self.held.clone()).is_ok() {
            events.extend(expired);
        }
    }

    /// A `(lane, kind)` episode stays alive while the lane keeps a
    /// waiter of that kind queued — `seen` tracks its freshest
    /// `last_poll` — and for `WAITER_TTL_SECS` after the lane's last
    /// poll, so a caller re-queueing under a new request_id inside
    /// that window keeps the lane's place. Past the TTL the lane has
    /// gone quiet: the next request anchors fresh. (The OTHER half
    /// of the rule lives in `grant`: a served lane's record is
    /// cleared even when waiters remain.)
    fn prune_seniority(&mut self, now: f64) {
        let waiting = &self.waiting;
        self.seniority.retain(|(lane, kind), s| {
            let live = waiting
                .iter()
                .filter(|w| w.lane == *lane && w.kind == *kind)
                .map(|w| w.last_poll)
                .reduce(f64::max);
            match live {
                Some(last_poll) => {
                    s.seen = last_poll;
                    true
                }
                None => now - s.seen <= WAITER_TTL_SECS,
            }
        });
    }

    /// The `queued_at` a NEW waiter takes. Only the lane's eldest
    /// unserved waiter of a kind inherits the episode anchor — later
    /// arrivals of a burst stamp their own arrival and queue behind
    /// it, so a lane cannot multiply one anchor into N front-runners.
    /// The inherited stamp is clamped to the starvation bound HERE,
    /// at stamp time: an episode older than `starve_secs` ranks
    /// starved but never ahead of a genuinely older starved waiter.
    fn stamp_for(&mut self, lane: &str, kind: SlotKind, now: f64) -> f64 {
        let key = (lane.to_string(), kind);
        if self
            .waiting
            .iter()
            .any(|w| w.lane == lane && w.kind == kind)
        {
            // Behind the lane's eldest waiter — the anchor is not
            // yours to carry.
            return now;
        }
        let anchor = self
            .seniority
            .get(&key)
            .map(|s| s.anchor)
            .unwrap_or(now)
            .max(now - self.config.starve_secs as f64);
        self.seniority.insert(key, Seniority { anchor, seen: now });
        anchor
    }

    /// What `stamp_for` would answer, without touching the episode —
    /// a probe is read-only and must never arm an anchor for a lane
    /// that hasn't actually queued.
    fn peek_stamp(&self, lane: &str, kind: SlotKind, now: f64) -> f64 {
        if self
            .waiting
            .iter()
            .any(|w| w.lane == lane && w.kind == kind)
        {
            return now;
        }
        self.seniority
            .get(&(lane.to_string(), kind))
            .map(|s| s.anchor)
            .unwrap_or(now)
            .max(now - self.config.starve_secs as f64)
    }

    /// Atomic best-effort write of the hold registry — a reader never
    /// sees a torn file, and a write failure is logged, not fatal:
    /// the in-memory registry stays authoritative either way. The tmp
    /// file is fsynced before the rename and the directory after it,
    /// so a crash cannot lose a persisted hold silently. Seniority is
    /// deliberately not persisted: it measures a wait, and no waiter
    /// survives a restart — every caller re-polls into a fresh anchor.
    ///
    /// This is the legacy writer, but it serializes the WHOLE state:
    /// once any strict record exists (or the file already is v2) it
    /// writes the v2 envelope with every enrollment and strict hold,
    /// so a legacy grant, release or reap can never drop strict state.
    /// A rejected state file is never overwritten — it is evidence.
    /// Returns whether the file now matches memory.
    fn persist(&mut self) -> bool {
        let Some(path) = self.persist_path.clone() else {
            return true;
        };
        if self.blocked.as_ref().is_some_and(|b| b.preserve_file) {
            return false;
        }
        let v2 =
            self.v2 || !self.enrollments.is_empty() || self.held.iter().any(|h| h.strict.is_some());
        let doc = if v2 {
            Self::v2_doc(&self.enrollments, &self.held, self.state_generation + 1)
        } else {
            json!({
                "version": 1,
                "holds": self.held.iter().map(SlotHold::legacy_json).collect::<Vec<_>>(),
            })
        };
        match write_atomic(&path, &doc) {
            Ok(()) => {
                if v2 {
                    self.v2 = true;
                    self.state_generation += 1;
                }
                true
            }
            Err(e) => {
                eprintln!("slots: persist {} failed: {e}", path.display());
                false
            }
        }
    }

    /// The v2 envelope: enrollments, strict holds (each naming its
    /// enrollment), and every legacy hold under `legacy_holds`.
    fn v2_doc(enrollments: &[Enrollment], held: &[SlotHold], generation: u64) -> Value {
        json!({
            "format": "cadence-slots",
            "version": 2,
            "state_generation": generation.to_string(),
            "enrollments": enrollments.iter().map(Enrollment::to_json).collect::<Vec<_>>(),
            "holds": held.iter().filter_map(|h| {
                let b = h.strict.as_ref()?;
                let mut row = json!({
                    "token": h.token, "request_id": h.request_id,
                    "kind": h.kind.as_str(), "lane": h.lane,
                    "enrollment_id": b.enrollment_id,
                    "owner_generation": b.owner_generation,
                    "holder": b.holder.to_json(),
                    "acquired_epoch": h.acquired_epoch,
                });
                // Additive (CAD-230b): absent reads as not exec-bound.
                if b.exec_bound {
                    row["exec_bound"] = json!(true);
                }
                Some(row)
            }).collect::<Vec<_>>(),
            "legacy_holds": held.iter()
                .filter(|h| h.strict.is_none())
                .map(SlotHold::legacy_json)
                .collect::<Vec<_>>(),
        })
    }

    /// The fail-closed strict writer. The next state is written FIRST
    /// and swapped into memory only once the write landed: a failure
    /// leaves disk and memory exactly as they were — no grant, no free
    /// — and makes strict admission unavailable until a restart loads
    /// a good file.
    ///
    /// A revoked or expired enrollment is a TOMBSTONE: it stays while
    /// any hold names it or while its root process is alive or
    /// unknown, because [`Self::nearest_enrolled_root`] must keep
    /// seeing that exact root — otherwise its processes would fall
    /// through to an outer pane's legacy binding. It is pruned only
    /// once nothing holds under it and its root is proven dead.
    fn commit(&mut self, enrollments: Vec<Enrollment>, held: Vec<SlotHold>) -> Result<()> {
        if let Some(b) = &self.blocked {
            return Err(strict_unavailable(&b.reason));
        }
        let enrollments: Vec<Enrollment> = enrollments
            .into_iter()
            .filter(|e| {
                e.auth == AuthState::Active
                    || held
                        .iter()
                        .any(|h| h.enrollment_id() == Some(e.id.as_str()))
                    || self.proc.liveness(&e.root).0 != Liveness::Dead
            })
            .collect();
        if let Some(path) = self.persist_path.clone() {
            let generation = self.state_generation + 1;
            if let Err(e) = write_atomic(&path, &Self::v2_doc(&enrollments, &held, generation)) {
                let reason = format!("strict state write to {} failed: {e}", path.display());
                eprintln!("slots: {reason}");
                self.blocked = Some(Blocked {
                    reason: reason.clone(),
                    preserve_file: false,
                });
                return Err(strict_unavailable(&reason));
            }
            self.v2 = true;
            self.state_generation = generation;
        }
        self.enrollments = enrollments;
        self.held = held;
        Ok(())
    }

    /// The state file itself is unusable: keep it untouched as
    /// evidence and refuse strict admission.
    fn reject_file(&mut self, reason: String) {
        eprintln!("slots: strict admission unavailable — {reason}");
        self.blocked = Some(Blocked {
            reason,
            preserve_file: true,
        });
    }

    /// Revalidate persisted holds at daemon start: a hold survives
    /// only while its recorded process is still the same live
    /// process. Survivors keep their tokens (in-flight releases and
    /// re-polls still resolve); the dead are dropped with a named
    /// reason rather than silently re-granted. Returns the boot-time
    /// release events for the daemon to emit.
    ///
    /// Order (CAD-230): the envelope is validated whole before any of
    /// it is used — unknown version, malformed JSON or schema, and
    /// duplicate records reject the file (kept as evidence, strict
    /// admission unavailable). Then enrollments (expiry, root identity)
    /// before strict holds (tri-state: alive retains, dead frees,
    /// unknown stays accounted) before legacy holds (unchanged rules).
    /// Waiters are never persisted, so none is ever replayed.
    pub fn restore(&mut self, clk: SlotClock) -> Vec<SlotEvent> {
        let mut events = Vec::new();
        let Some(path) = self.persist_path.clone() else {
            return events;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return events,
            Err(e) => {
                self.reject_file(format!("cannot read {}: {e}", path.display()));
                return events;
            }
        };
        let Ok(doc) = serde_json::from_str::<Value>(&text) else {
            self.reject_file(format!("{} is unparsable", path.display()));
            return events;
        };
        let loaded = match (doc.get("format"), doc.get("version")) {
            (Some(format), Some(version)) if format == "cadence-slots" && version == 2 => {
                self.restore_v2(&doc, clk, &mut events)
            }
            (Some(format), version) => Err(format!(
                "unknown envelope (format {format}, version {})",
                version.cloned().unwrap_or(Value::Null)
            )),
            (None, Some(version)) if version != 1 => Err(format!("unknown version {version}")),
            // Anything that is not a well-formed envelope — `{}`,
            // `null`, `[]`, a non-list `holds` — is malformed state:
            // kept as evidence, never rewritten, strict blocked.
            (None, _) if !doc.is_object() || !doc["holds"].is_array() => {
                Err("not a well-formed v1 envelope (an object with a 'holds' list)".to_string())
            }
            // The v1 shape — or a version-less file an older writer
            // left: the legacy rows, under the unchanged rules.
            (None, _) => {
                self.restore_legacy(&doc["holds"], clk, &mut events);
                Ok(())
            }
        };
        if let Err(why) = loaded {
            self.reject_file(format!("{}: {why}", path.display()));
            return events;
        }
        // Seniority is never persisted: it measures an unserved wait
        // and no waiter survives a restart — re-polling callers
        // anchor fresh. (A stale `seniority` block in an old file is
        // simply ignored.) A boot rewrite that fails issues no strict
        // grant until a later restart writes cleanly.
        if !self.persist() && self.blocked.is_none() {
            self.blocked = Some(Blocked {
                reason: format!("boot rewrite of {} failed", path.display()),
                preserve_file: false,
            });
        }
        events
    }

    /// Legacy (v1 / `legacy_holds`) rows — the CAD-113 rules exactly:
    /// the alive-only fallback, per-row skips, dead holders reaped.
    fn restore_legacy(&mut self, holds: &Value, clk: SlotClock, events: &mut Vec<SlotEvent>) {
        let (now, wall) = (clk.mono, clk.wall);
        for h in holds.as_array().cloned().unwrap_or_default() {
            let (Some(kind), Some(lane), Some(pid)) = (
                h["kind"].as_str().and_then(|k| SlotKind::parse(k).ok()),
                h["lane"].as_str(),
                h["pid"].as_u64().map(|p| p as u32),
            ) else {
                continue;
            };
            let start = h["pid_start"].as_u64();
            let held_secs = (wall - h["acquired_epoch"].as_f64().unwrap_or(wall)).max(0.0);
            if !pid_matches(pid, start) {
                let reason = if pid_alive(pid) {
                    "pid recycled"
                } else {
                    "holder died"
                };
                events.push((
                    lane.to_string(),
                    "slot_released",
                    json!({"kind": kind.as_str(), "pid": pid,
                           "held_secs": held_secs, "reason": reason}),
                ));
                continue;
            }
            let Some(token) = h["token"].as_str().map(str::to_string) else {
                eprintln!(
                    "slots: dropping persisted hold with no token \
                     ({lane} {kind:?} pid {pid}) — persist file truncated?"
                );
                continue;
            };
            self.held.push(SlotHold {
                token,
                request_id: h["request_id"].as_str().unwrap_or_default().to_string(),
                kind,
                lane: lane.to_string(),
                pid,
                pid_start: start,
                acquired_at: now - held_secs,
                acquired_epoch: wall - held_secs,
                strict: None,
            });
        }
    }

    /// Validate the whole v2 envelope, then apply it in order. Any
    /// schema fault — or a same-root enrollment pair across owners, in
    /// any authorization state — is an `Err` before anything is applied.
    fn restore_v2(
        &mut self,
        doc: &Value,
        clk: SlotClock,
        events: &mut Vec<SlotEvent>,
    ) -> std::result::Result<(), String> {
        let (now, wall) = (clk.mono, clk.wall);
        let generation: u64 = doc["state_generation"]
            .as_str()
            .and_then(|g| g.parse().ok())
            .ok_or("state_generation missing or malformed")?;
        let list = |key: &str| {
            doc[key]
                .as_array()
                .cloned()
                .ok_or_else(|| format!("'{key}' missing or not a list"))
        };
        let mut enrollments = Vec::new();
        for e in list("enrollments")? {
            let parsed = Enrollment::from_json(&e, now, wall)
                .ok_or_else(|| format!("malformed enrollment {e}"))?;
            // Ids are unique. A shared root of ONE owner never rejects
            // the file (CAD-276) — a same-root supersession, which
            // `strict_caller` resolves deterministically — but across
            // owners, in any authorization state, the pair has no
            // lineage: the daemon never writes it (`enroll` refuses a
            // process enrolled for another owner) and the file is
            // invalid (CAD-289).
            if enrollments.iter().any(|o: &Enrollment| o.id == parsed.id) {
                return Err(format!("duplicate enrollment {}", parsed.id));
            }
            if let Some(o) = enrollments.iter().find(|o: &&Enrollment| {
                o.root == parsed.root && o.owner_actor != parsed.owner_actor
            }) {
                return Err(format!(
                    "enrollments {} and {} share root pid {} across owners ({} / {}) \
                     — one provider process is enrolled for one owner",
                    o.id, parsed.id, parsed.root.pid, o.owner_actor, parsed.owner_actor
                ));
            }
            enrollments.push(parsed);
        }
        let mut tokens = std::collections::HashSet::new();
        let mut strict_rows = Vec::new();
        for h in list("holds")? {
            let row = (|| {
                let enrollment_id = h["enrollment_id"].as_str()?.to_string();
                let lane = enrollments
                    .iter()
                    .find(|e| e.id == enrollment_id)?
                    .owner_actor
                    .clone();
                Some((
                    h["token"].as_str()?.to_string(),
                    h["request_id"].as_str()?.to_string(),
                    SlotKind::parse(h["kind"].as_str()?).ok()?,
                    lane,
                    StrictBind {
                        enrollment_id,
                        owner_generation: h["owner_generation"].as_str()?.to_string(),
                        holder: ProcIdentity::from_json(&h["holder"])?,
                        liveness: Liveness::Unknown,
                        exec_bound: match h.get("exec_bound") {
                            None => false,
                            Some(v) => v.as_bool()?,
                        },
                    },
                    h["acquired_epoch"].as_f64()?,
                ))
            })()
            .ok_or_else(|| format!("malformed or unbound strict hold {h}"))?;
            if !tokens.insert(row.0.clone()) {
                return Err(format!("duplicate hold token {}", row.0));
            }
            strict_rows.push(row);
        }
        let legacy = list("legacy_holds")?;
        for h in &legacy {
            let ok = h["token"].as_str().is_some()
                && h["kind"]
                    .as_str()
                    .and_then(|k| SlotKind::parse(k).ok())
                    .is_some()
                && h["lane"].as_str().is_some()
                && h["pid"].as_u64().is_some()
                && h["acquired_epoch"].as_f64().is_some();
            if !ok {
                return Err(format!("malformed legacy hold {h}"));
            }
            if !tokens.insert(h["token"].as_str().unwrap_or_default().to_string()) {
                return Err(format!("duplicate hold token {}", h["token"]));
            }
        }
        // Validated — apply: enrollments, then strict holds, then
        // legacy holds.
        self.v2 = true;
        self.state_generation = generation;
        for e in &mut enrollments {
            if e.auth == AuthState::Active && wall >= e.expires_epoch {
                e.auth = AuthState::Expired;
            }
            if matches!(e.auth, AuthState::Revoked(_)) {
                continue;
            }
            // A runner's owner is the previous daemon's in-memory
            // runner record (CAD-230b): it did not survive the restart,
            // so the enrollment admits nothing more. Its hold stays
            // accounted under the tri-state rule below until the
            // process is proven dead — it is never relaunched.
            if e.runner.is_some() {
                e.auth = AuthState::Revoked(
                    "daemon restarted — runner outcome unknown, never relaunched".into(),
                );
                continue;
            }
            match self.proc.liveness(&e.root).0 {
                Liveness::Alive => {}
                Liveness::Dead => {
                    e.auth = AuthState::Revoked("root process gone at restart".into())
                }
                Liveness::Unknown => {
                    e.auth = AuthState::Revoked("root liveness unknown at restart".into())
                }
            }
        }
        self.enrollments = enrollments;
        for (token, request_id, kind, lane, mut bind, acquired_epoch) in strict_rows {
            let held_secs = (wall - acquired_epoch).max(0.0);
            let (liveness, why) = self.proc.liveness(&bind.holder);
            if liveness == Liveness::Dead {
                events.push((
                    lane,
                    "slot_released",
                    json!({"kind": kind.as_str(), "pid": bind.holder.pid,
                           "held_secs": held_secs, "reason": why}),
                ));
                continue;
            }
            bind.liveness = liveness;
            self.held.push(SlotHold {
                token,
                request_id,
                kind,
                lane,
                pid: bind.holder.pid,
                pid_start: Some(bind.holder.starttime),
                acquired_at: now - held_secs,
                acquired_epoch: wall - held_secs,
                strict: Some(bind),
            });
        }
        self.restore_legacy(&Value::Array(legacy), clk, events);
        Ok(())
    }

    /// A hold's identity is (request_id, pid, lane, kind): only the
    /// exact same caller re-polling adopts the grant — another
    /// process sharing a natural request id is a different caller.
    fn find_hold(&self, req: &SlotReq<'_>) -> Option<&SlotHold> {
        self.held.iter().find(|h| {
            h.request_id == req.request_id
                && h.pid == req.pid
                && h.lane == req.lane
                && h.kind == req.kind
                && h.enrollment_id() == req.enrollment_id()
                // A strict hold is adopted only by its exact recorded
                // holder — a recycled pid (new starttime) is a
                // different process and never re-binds it (CAD-230b).
                && h.strict.as_ref().map(|b| b.holder) == req.strict.as_ref().map(|b| b.holder)
        })
    }

    /// The grant itself: hold registration + the `slot_acquired`
    /// event, shared by the probe and the queueing path. The token is
    /// minted here — the caller's request_id is queue identity only.
    /// The event carries NO token: lane event streams are readable by
    /// any local caller, and token+lane+pid are exactly the inputs a
    /// release authenticates — publishing all three would hand every
    /// peer the keys to a live hold.
    ///
    /// A strict grant goes through the fail-closed writer: when the
    /// write fails nothing is granted and the caller is refused.
    fn grant(
        &mut self,
        req: SlotReq<'_>,
        wait_secs: f64,
        clk: SlotClock,
        events: &mut Vec<SlotEvent>,
    ) -> Result<Value> {
        let (now, wall) = (clk.mono, clk.wall);
        let token = format!("slot-{}", Uuid::new_v4().simple());
        let hold = SlotHold {
            token: token.clone(),
            request_id: req.request_id.to_string(),
            kind: req.kind,
            lane: req.lane.to_string(),
            pid: req.pid,
            pid_start: match &req.strict {
                Some(b) => Some(b.holder.starttime),
                None => pid_start(req.pid),
            },
            acquired_at: now,
            acquired_epoch: wall,
            strict: req.strict.clone(),
        };
        if hold.strict.is_some() {
            let mut next = self.held.clone();
            next.push(hold);
            self.commit(self.enrollments.clone(), next)?;
        } else {
            self.held.push(hold);
            self.persist();
        }
        events.push((
            req.lane.to_string(),
            "slot_acquired",
            json!({"kind": req.kind.as_str(),
                   "pool": req.kind.pool().as_str(), "wait_secs": wait_secs,
                   "pid": req.pid}),
        ));
        // Serving this lane ends its unserved wait: the seniority
        // anchor dies here so a lane that always has "one more
        // request" queued can never ride an ancient timestamp — its
        // next enqueue anchors at its own honest arrival.
        self.seniority.remove(&(req.lane.to_string(), req.kind));
        Ok(json!({"granted": true, "token": token,
               "kind": req.kind.as_str(), "wait_secs": wait_secs}))
    }

    /// Cross-pool deadlock guard: a caller may never QUEUE for one
    /// pool while holding a slot in the other — otherwise A(holds
    /// build, waits suite) vs B(holds suite, waits build) is a
    /// classic hold-and-wait deadlock. The guard keys on `(lane,
    /// pid)` — the holding *process*: two unrelated shells sharing a
    /// lane name (e.g. `$USER` when no `CADENCE_ALIAS` is set) never
    /// block each other. Granting without waiting is always fine: a
    /// caller that never waits holds no wait-edge.
    fn holds_other_pool(&self, pool: Pool, lane: &str, pid: u32) -> bool {
        self.held
            .iter()
            .any(|h| h.lane == lane && h.pid == pid && h.kind.pool() != pool)
    }

    /// Non-blocking acquire: grants a minted token when the pool has
    /// room and the request leads its effective order, else reports
    /// the queue position. `request_id` makes client polls sticky —
    /// a re-poll with the SAME (pid, lane, kind) keeps the original
    /// place and refreshes `last_poll` (the abandonment signal); a
    /// re-poll with a different identity is a different caller and
    /// queues on its own. `probe` answers the same question WITHOUT
    /// leaving a waiter behind — the `--wait-secs 0` fast-fail.
    pub fn acquire(
        &mut self,
        kind: SlotKind,
        lane: &str,
        pid: u32,
        request_id: &str,
        probe: bool,
        clk: SlotClock,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        self.acquire_as(kind, lane, pid, request_id, probe, clk, None)
    }

    /// [`Self::acquire`] under either binding — `strict` carries the
    /// verified enrollment binding of a strict caller.
    #[allow(clippy::too_many_arguments)]
    fn acquire_as(
        &mut self,
        kind: SlotKind,
        lane: &str,
        pid: u32,
        request_id: &str,
        probe: bool,
        clk: SlotClock,
        strict: Option<StrictBind>,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        let mut events = Vec::new();
        let now = clk.mono;
        if pid == 0 {
            return Err(Error::rejected(
                "Slot acquire needs the holder's pid — `build-slot run` binds \
                 the real command; a manual `acquire` passes `--pid $$`",
            ));
        }
        self.reap_dead(now, &mut events);
        let req = SlotReq {
            kind,
            lane,
            pid,
            request_id,
            strict,
        };
        let enrollment = req.enrollment_id().map(str::to_string);
        let same = |w: &SlotWait| {
            w.request_id == request_id
                && w.pid == pid
                && w.lane == lane
                && w.kind == kind
                && w.strict.as_ref().map(|b| b.enrollment_id.as_str()) == enrollment.as_deref()
        };
        // Idempotent re-poll: the same caller re-asking for its grant
        // gets the same minted token back. Anything sharing only the
        // request_id is a different caller — it queues below, never
        // adopts this hold.
        if let Some(h) = self.find_hold(&req) {
            return Ok((
                json!({"granted": true, "token": h.token,
                       "kind": h.kind.as_str()}),
                events,
            ));
        }
        let pool = kind.pool();
        if probe {
            // Already queued? A probe is read-only — it reports the
            // existing waiter's real position, never grants it (a
            // grant is a mutation) and never re-ranks it fresh.
            if let Some(idx) = self.waiting.iter().position(same) {
                let ranks = self.ranks(now);
                return Ok((
                    json!({"granted": false, "position": self.outranked(&ranks, idx) + 1,
                           "wait_secs": (now - self.waiting[idx].queued_at).max(0.0),
                           "held": self.held_in(pool),
                           "capacity": self.capacity(pool)}),
                    events,
                ));
            }
            // A fresh probe grants exactly when an enqueue would —
            // capacity free and the request next — but never joins
            // the queue and never touches seniority (`peek_stamp`
            // only reads).
            let senior = self.peek_stamp(lane, kind, now);
            let w = SlotWait {
                request_id: request_id.to_string(),
                kind,
                lane: lane.to_string(),
                pid,
                pid_start: req.pid_start(),
                queued_at: senior,
                last_poll: now,
                strict: req.strict.clone(),
            };
            self.waiting.push(w);
            let ranks = self.ranks(now);
            let idx = self.waiting.len() - 1;
            let next = self.held_in(pool) < self.capacity(pool) && self.outranked(&ranks, idx) == 0;
            let position = self.outranked(&ranks, idx) + 1;
            self.waiting.pop();
            self.prune_seniority(now);
            if next {
                return Ok((self.grant(req, 0.0, clk, &mut events)?, events));
            }
            if self.holds_other_pool(pool, lane, pid) {
                return Err(Error::rejected(format!(
                    "A caller holding a {} slot cannot queue for {} — \
                     release that hold first (hold-and-wait deadlock guard)",
                    other_pool(pool).as_str(),
                    pool.as_str(),
                )));
            }
            return Ok((
                json!({"granted": false, "position": position,
                       "held": self.held_in(pool),
                       "capacity": self.capacity(pool)}),
                events,
            ));
        }
        let (idx, created) = match self.waiting.iter().position(same) {
            Some(i) => (i, false),
            None => {
                if self.waiting.len() >= MAX_WAITERS {
                    return Err(Error::rejected(format!(
                        "Slot queue is full ({MAX_WAITERS} waiting) — try again later"
                    )));
                }
                // A lane's share is bounded too — without it one lane
                // could fill the whole queue and deny everyone else.
                if self.waiting.iter().filter(|w| w.lane == lane).count() >= MAX_WAITERS_PER_LANE {
                    return Err(Error::rejected(format!(
                        "Lane '{lane}' already has {MAX_WAITERS_PER_LANE} slot \
                         requests queued — let some grant or die first"
                    )));
                }
                let senior = self.stamp_for(lane, kind, now);
                self.waiting.push(SlotWait {
                    request_id: request_id.to_string(),
                    kind,
                    lane: lane.to_string(),
                    pid,
                    pid_start: req.pid_start(),
                    queued_at: senior,
                    last_poll: now,
                    strict: req.strict.clone(),
                });
                (self.waiting.len() - 1, true)
            }
        };
        // The liveness stamp every poll owes — identity fields are
        // the match key, so a re-poll only refreshes this (and the
        // episode's last-seen, so gap grace runs from the lane's
        // real last poll rather than the last reap pass).
        self.waiting[idx].last_poll = now;
        if let Some(s) = self.seniority.get_mut(&(lane.to_string(), kind)) {
            s.seen = s.seen.max(now);
        }

        let ranks = self.ranks(now);
        if self.held_in(pool) < self.capacity(pool) && self.outranked(&ranks, idx) == 0 {
            let wait_secs = (now - self.waiting[idx].queued_at).max(0.0);
            // Grant first: a strict grant whose write fails leaves the
            // caller queued exactly where it was.
            let granted = self.grant(req, wait_secs, clk, &mut events)?;
            self.waiting.remove(idx);
            self.prune_seniority(now);
            return Ok((granted, events));
        }
        // It must wait — but a caller holding the other pool may not
        // queue at all (the deadlock guard). The forbidden waiter
        // is removed, never registered.
        if self.holds_other_pool(pool, lane, pid) {
            self.waiting.remove(idx);
            self.prune_seniority(now);
            return Err(Error::rejected(format!(
                "A caller holding a {} slot cannot queue for {} — \
                 release that hold first (hold-and-wait deadlock guard)",
                other_pool(pool).as_str(),
                pool.as_str(),
            )));
        }
        let position = self.outranked(&ranks, idx) + 1;
        let wait_secs = (now - self.waiting[idx].queued_at).max(0.0);
        if created {
            // First poll and already queueing — tell observers the
            // wait started (re-polls don't repeat the event).
            events.push((
                lane.to_string(),
                "slot_waited",
                json!({"request_id": request_id, "kind": kind.as_str(),
                       "pool": pool.as_str(), "pid": pid}),
            ));
        }
        Ok((
            json!({"granted": false, "position": position,
                   "wait_secs": wait_secs,
                   "held": self.held_in(pool), "capacity": self.capacity(pool)}),
            events,
        ))
    }

    /// Return a held slot. The release must name the holding caller —
    /// (lane, pid) — so one caller can never release another's hold.
    /// A foreign token is a named refusal; an unknown token a named
    /// rejection — EXCEPT a token this very call just reaped, which
    /// answers softly (`released:false` + the reap reason) to the
    /// hold's own lane: a cleanup path like `trap 'release $T' EXIT`
    /// should not hard-fail on a hold the daemon already took back,
    /// and a foreign lane gets the hard unknown-token refusal so a
    /// token's existence is never confirmed across lanes.
    pub fn release(
        &mut self,
        token: &str,
        lane: &str,
        pid: u32,
        now: f64,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        self.release_as(token, lane, pid, now, None)
    }

    /// [`Self::release`] under either binding. A strict hold is freed
    /// only by a verified caller of its own enrollment naming the exact
    /// recorded holder (pid AND starttime) — through the fail-closed
    /// writer, so a failed write keeps it held. The two bindings never
    /// release each other's holds.
    fn release_as(
        &mut self,
        token: &str,
        lane: &str,
        pid: u32,
        now: f64,
        strict: Option<&StrictCaller>,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        let mut events = Vec::new();
        let reaped = self.reap_dead(now, &mut events);
        let Some(h) = self.held.iter().find(|h| h.token == token) else {
            if let Some(r) = reaped
                .iter()
                .chain(self.recent_reaped.iter())
                .find(|r| r.token == token && r.lane == lane)
            {
                return Ok((
                    json!({"released": false, "token": token,
                           "kind": r.kind.as_str(), "reason": r.reason}),
                    events,
                ));
            }
            return Err(Error::rejected(format!(
                "Unknown slot token '{token}' — it was never granted or already released"
            )));
        };
        let exact = match (&h.strict, strict) {
            (None, None) => true,
            (Some(b), Some(c)) => {
                b.enrollment_id == c.enrollment_id
                    && c.segment.contains(&pid)
                    && self.proc.identity(pid).is_ok_and(|now| now == b.holder)
            }
            _ => false,
        };
        if h.lane != lane || h.pid != pid || !exact {
            return Err(Error::rejected(
                "Slot token is held by another caller — release must come \
                 from the holding lane and pid",
            ));
        }
        let idx = self.held.iter().position(|h| h.token == token).unwrap();
        let h = if self.held[idx].strict.is_some() {
            let mut next = self.held.clone();
            let h = next.remove(idx);
            self.commit(self.enrollments.clone(), next)?;
            h
        } else {
            let h = self.held.remove(idx);
            self.persist();
            h
        };
        events.push((
            h.lane.clone(),
            "slot_released",
            json!({"kind": h.kind.as_str(), "pid": h.pid,
                   "held_secs": (now - h.acquired_at).max(0.0),
                   "reason": "released"}),
        ));
        Ok((
            json!({"released": true, "token": token, "kind": h.kind.as_str()}),
            events,
        ))
    }

    /// Everything `cadence status`, `doctor --host` and the job runner
    /// need: per-pool capacity and holders, the live queue with wait
    /// ages, and the resolved config. A status read is also a reap
    /// pass — a dead holder frees its slot on the next read, not some
    /// later sweep, so the queue can never jam behind a corpse.
    /// `caller` is the connection-derived identity: a hold's token
    /// shows only when the caller IS the holder — same lane and the
    /// hold's pid on the caller's own chain. Other processes see
    /// identity only — a token never leaves its owner's view.
    pub fn status(&mut self, caller: SlotCaller<'_>, now: f64) -> (Value, Vec<SlotEvent>) {
        let mut events = Vec::new();
        self.reap_dead(now, &mut events);
        (self.status_json(caller, now), events)
    }

    fn status_json(&self, caller: SlotCaller<'_>, now: f64) -> Value {
        let pool_json = |pool: Pool| {
            json!({
                "capacity": self.capacity(pool),
                "held": self.held.iter()
                    .filter(|h| h.kind.pool() == pool)
                    .map(|h| {
                        let mut j = json!({"kind": h.kind.as_str(),
                                    "lane": h.lane, "pid": h.pid,
                                    "age_secs": (now - h.acquired_at).max(0.0)});
                        if h.lane == caller.lane && caller.pids.contains(&h.pid) {
                            j["token"] = json!(h.token);
                        }
                        self.strict_hold_json(h, now, &mut j);
                        j
                    })
                    .collect::<Vec<_>>(),
            })
        };
        let ranks = self.ranks(now);
        let mut order: Vec<usize> = (0..self.waiting.len()).collect();
        order.sort_by_key(|i| (ranks[*i].0, (ranks[*i].1 * 1e6) as i64));
        json!({
            "pools": {
                "build": pool_json(Pool::Build),
                "suite": pool_json(Pool::Suite),
            },
            "waiting": order.iter().map(|i| {
                let w = &self.waiting[*i];
                let (rank, _) = ranks[*i];
                json!({"request_id": w.request_id, "kind": w.kind.as_str(),
                       "lane": w.lane, "pid": w.pid,
                       "wait_secs": (now - w.queued_at).max(0.0),
                       "starved": rank == 0, "priority": rank == 1})
            }).collect::<Vec<_>>(),
            "config": {
                "build_slots": self.config.build_slots,
                "suite_slots": self.config.suite_slots,
                "jobs_per_lane": self.config.jobs_per_lane,
                "starve_secs": self.config.starve_secs,
                "priority_lanes": self.config.priority_lanes,
                "max_hold_secs": self.config.max_hold_secs,
            },
            "enrollments": self.enrollments.iter().map(Enrollment::to_json).collect::<Vec<_>>(),
            "strict": match &self.blocked {
                None => json!({"available": true,
                               "state_generation": self.state_generation}),
                Some(b) => json!({"available": false, "reason": b.reason,
                                  "reconcile_required": true,
                                  "state_generation": self.state_generation}),
            },
        })
    }

    /// A strict hold's accounting: `held` inside `max_hold_secs`,
    /// `expired_pending_reconcile` past it — still occupying its slot,
    /// because only an exact release or proven death frees one.
    fn accounting(&self, h: &SlotHold, now: f64) -> &'static str {
        if now - h.acquired_at >= self.config.max_hold_secs as f64 {
            "expired_pending_reconcile"
        } else {
            "held"
        }
    }

    /// Status fields of a hold's binding: `legacy`, or `strict` with
    /// its enrollment's authorization kept separate from the hold's own
    /// liveness and accounting, plus `reconcile_required` / `remedy`.
    fn strict_hold_json(&self, h: &SlotHold, now: f64, j: &mut Value) {
        let Some(b) = &h.strict else {
            j["binding"] = json!("legacy");
            return;
        };
        let auth = self
            .enrollments
            .iter()
            .find(|e| e.id == b.enrollment_id)
            .map(|e| e.auth.as_str())
            .unwrap_or("revoked");
        let accounting = self.accounting(h, now);
        j["binding"] = json!("strict");
        if b.exec_bound {
            j["exec_bound"] = json!(true);
        }
        j["enrollment_id"] = json!(b.enrollment_id);
        j["owner_generation"] = json!(b.owner_generation);
        j["auth_state"] = json!(auth);
        j["liveness"] = json!(b.liveness.as_str());
        j["accounting"] = json!(accounting);
        // `reconcile_required` only where `slot_reconcile` can act — a
        // holder proven dead whose free has not landed yet, while the
        // strict writer (which reconcile frees through) is available
        // (CAD-276). A live or unknown holder is refused by reconcile
        // whatever the evidence says, and a blocked writer refuses
        // every strict write, so those name the remedy that does work.
        let writable = self.blocked.is_none();
        let remedy = match b.liveness {
            Liveness::Dead if writable => None,
            Liveness::Dead => Some(
                "holder dead but strict state is unwritable — restart the daemon \
                 once slots.json is writable",
            ),
            Liveness::Unknown => Some("liveness unknown — restart the daemon after verifying"),
            Liveness::Alive if accounting != "held" => {
                Some("holder alive — release from the holder or stop it")
            }
            Liveness::Alive => None,
        };
        j["reconcile_required"] = json!(b.liveness == Liveness::Dead && writable);
        if let Some(remedy) = remedy {
            j["remedy"] = json!(remedy);
        }
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
        if let Some(b) = &self.blocked {
            return Err(strict_unavailable(&b.reason));
        }
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
        let mut owners: Vec<String> = self
            .enrollments
            .iter()
            .filter(|e| !matches!(e.auth, AuthState::Revoked(_)) && e.runner.is_none())
            .map(|e| e.owner_actor.clone())
            .collect();
        owners.sort();
        owners.dedup();
        owners
    }

    /// Revalidate each unrevoked enrollment against its owner row as the
    /// daemon reads it now (`current`: owner → its generation, `None`
    /// when the row is gone or has no live endpoint). A missing or
    /// changed generation revokes — fail closed.
    pub fn revalidate_owners(
        &mut self,
        current: &HashMap<String, Option<String>>,
    ) -> Vec<SlotEvent> {
        // A runner's owner is the daemon's runner record, not an agent
        // row — it is never revalidated against one (CAD-230b).
        let drifted = |e: &Enrollment| {
            e.runner.is_none()
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
    pub fn strict_caller(&self, peer_pid: u32, root_pid: u32) -> Result<StrictCaller> {
        if let Some(b) = &self.blocked {
            return Err(strict_unavailable(&b.reason));
        }
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
        if let Some(b) = &self.blocked {
            return Err(strict_unavailable(&b.reason));
        }
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
        if let Some(b) = &self.blocked {
            return Err(strict_unavailable(&b.reason));
        }
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
        if let Some(b) = &self.blocked {
            return Err(strict_unavailable(&b.reason));
        }
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
        if let Some(b) = &self.blocked {
            return Err(strict_unavailable(&b.reason));
        }
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

/// Atomic write: tmp file (mode 0600) fsynced before the rename, the
/// directory after it, so a crash cannot lose or tear a record.
fn write_atomic(path: &Path, doc: &Value) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(doc.to_string().as_bytes())?;
            f.sync_all()
        })
        .and_then(|_| std::fs::rename(&tmp, path))?;
    // The rename is durable only once its directory is.
    if let Some(dir) = path.parent() {
        let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(build: usize, suite: usize, starve: u64, priority: &[&str]) -> Slots {
        Slots::new(SlotConfig {
            build_slots: build,
            suite_slots: suite,
            starve_secs: starve,
            priority_lanes: priority.iter().map(|s| s.to_string()).collect(),
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
    /// contention tests hand the slot between lanes.
    fn release_any(s: &mut Slots, now: f64) -> Value {
        let h = &s.held[0];
        let (token, lane) = (h.token.clone(), h.lane.clone());
        release(s, &token, &lane, now)
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
        // And a different kind under the same request_id also queues.
        let c = acquire(&mut s, SlotKind::Test, "dev-1", "r1", 0.0);
        assert_eq!(c["granted"], false);
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
        // now they stamp their own arrival.
        for req in ["w2", "w3", "w4"] {
            acquire(&mut s, SlotKind::Build, "dev-2", req, 10.0);
        }
        release_first(&mut s, "dev-1", 20.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w1", 20.0);
        assert_eq!(g["granted"], true, "the eldest keeps the anchor");
        release_first(&mut s, "dev-2", 21.0);
        let g = acquire(&mut s, SlotKind::Build, "qa-1", "qb", 21.0);
        assert_eq!(g["granted"], true, "qa-1's t=5 beats the t=10 burst");
        release_first(&mut s, "qa-1", 22.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w2", 22.0);
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
        for lane in 0..4 {
            for i in 0..MAX_WAITERS_PER_LANE {
                let q = acquire(
                    &mut s,
                    SlotKind::Build,
                    &format!("dev-w{lane}"),
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
        for i in 0..MAX_WAITERS_PER_LANE {
            let q = acquire(&mut s, SlotKind::Build, "dev-2", &format!("w{i}"), 0.0);
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
        let q = acquire(&mut s, SlotKind::Suite, "qa-1", "s2", 0.0);
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
        // waiters in flight riding the t=0 anchor.
        acquire(&mut s, SlotKind::Build, "A", "h", 0.0);
        acquire(&mut s, SlotKind::Build, "A", "w1", 0.0);
        acquire(&mut s, SlotKind::Build, "A", "w2", 0.0);
        // Lane B requests once, a second later.
        acquire(&mut s, SlotKind::Build, "B", "b1", 1.0);
        let mut a_waiters = vec!["w1".to_string(), "w2".to_string()];
        let mut next = 3u32;
        let mut b_granted_at = None;
        // Every 20s the holder releases; A's waiters poll first
        // (adversarial), B polls once, and A tops back up to two
        // in-flight requests — the monopolization pattern.
        for tick in 1..=45 {
            let t = tick as f64 * 20.0;
            release_any(&mut s, t);
            let mut still = Vec::new();
            for r in &a_waiters {
                let g = acquire(&mut s, SlotKind::Build, "A", r, t);
                if !g["granted"].as_bool().unwrap_or(false) {
                    still.push(r.clone());
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
                acquire(&mut s, SlotKind::Build, "A", &r, t);
                a_waiters.push(r);
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
}
