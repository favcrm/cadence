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

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{Error, Result};

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
}

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
/// unknown tokens.
struct Reaped {
    token: String,
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
}

impl Slots {
    pub fn new(config: SlotConfig) -> Self {
        Self {
            config,
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
    /// can answer a just-reaped token softly.
    fn reap_dead(&mut self, now: f64, events: &mut Vec<SlotEvent>) -> Vec<Reaped> {
        let mut dead: Vec<Reaped> = Vec::new();
        self.held.retain(|h| {
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
                kind: h.kind,
                reason,
            });
            false
        });
        // Waiters drop on either abandonment signal: a dead/recycled
        // pid, or a poll gone silent past the TTL (a fast-failed
        // --wait-secs 0 caller's pid may still be alive in its parent
        // — only the silence proves it walked away).
        self.waiting
            .retain(|w| pid_matches(w.pid, w.pid_start) && now - w.last_poll <= WAITER_TTL_SECS);
        self.prune_seniority(now);
        if !dead.is_empty() {
            self.persist();
        }
        dead
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
    fn persist(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };
        let doc = json!({
            "version": 1,
            "holds": self.held.iter().map(|h| json!({
                "token": h.token, "request_id": h.request_id,
                "kind": h.kind.as_str(), "lane": h.lane,
                "pid": h.pid, "pid_start": h.pid_start,
                "acquired_epoch": h.acquired_epoch,
            })).collect::<Vec<_>>(),
        });
        let tmp = path.with_extension("tmp");
        let write = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| {
                f.write_all(doc.to_string().as_bytes())?;
                f.sync_all()
            })
            .and_then(|_| std::fs::rename(&tmp, path));
        match write {
            Ok(()) => {
                // The rename is durable only once its directory is.
                if let Some(dir) = path.parent() {
                    let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
                }
            }
            Err(e) => eprintln!("slots: persist {} failed: {e}", path.display()),
        }
    }

    /// Revalidate persisted holds at daemon start: a hold survives
    /// only while its recorded process is still the same live
    /// process. Survivors keep their tokens (in-flight releases and
    /// re-polls still resolve); the dead are dropped with a named
    /// reason rather than silently re-granted. Returns the boot-time
    /// release events for the daemon to emit.
    pub fn restore(&mut self, clk: SlotClock) -> Vec<SlotEvent> {
        let mut events = Vec::new();
        let (now, wall) = (clk.mono, clk.wall);
        let Some(path) = &self.persist_path else {
            return events;
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return events;
        };
        let Ok(doc) = serde_json::from_str::<Value>(&text) else {
            eprintln!("slots: ignoring unparsable {}", path.display());
            return events;
        };
        for h in doc["holds"].as_array().cloned().unwrap_or_default() {
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
            });
        }
        // Seniority is never persisted: it measures an unserved wait
        // and no waiter survives a restart — re-polling callers
        // anchor fresh. (A stale `seniority` block in an old file is
        // simply ignored.)
        self.persist();
        events
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
        })
    }

    /// The grant itself: hold registration + the `slot_acquired`
    /// event, shared by the probe and the queueing path. The token is
    /// minted here — the caller's request_id is queue identity only.
    /// The event carries NO token: lane event streams are readable by
    /// any local caller, and token+lane+pid are exactly the inputs a
    /// release authenticates — publishing all three would hand every
    /// peer the keys to a live hold.
    fn grant(
        &mut self,
        req: SlotReq<'_>,
        wait_secs: f64,
        clk: SlotClock,
        events: &mut Vec<SlotEvent>,
    ) -> Value {
        let (now, wall) = (clk.mono, clk.wall);
        let token = format!("slot-{}", Uuid::new_v4().simple());
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
        self.held.push(SlotHold {
            token: token.clone(),
            request_id: req.request_id.to_string(),
            kind: req.kind,
            lane: req.lane.to_string(),
            pid: req.pid,
            pid_start: pid_start(req.pid),
            acquired_at: now,
            acquired_epoch: wall,
        });
        self.persist();
        json!({"granted": true, "token": token,
               "kind": req.kind.as_str(), "wait_secs": wait_secs})
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
            if let Some(idx) = self.waiting.iter().position(|w| {
                w.request_id == request_id && w.pid == pid && w.lane == lane && w.kind == kind
            }) {
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
                pid_start: pid_start(pid),
                queued_at: senior,
                last_poll: now,
            };
            self.waiting.push(w);
            let ranks = self.ranks(now);
            let idx = self.waiting.len() - 1;
            let next = self.held_in(pool) < self.capacity(pool) && self.outranked(&ranks, idx) == 0;
            let position = self.outranked(&ranks, idx) + 1;
            self.waiting.pop();
            self.prune_seniority(now);
            if next {
                return Ok((self.grant(req, 0.0, clk, &mut events), events));
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
        let (idx, created) = match self.waiting.iter().position(|w| {
            w.request_id == request_id && w.pid == pid && w.lane == lane && w.kind == kind
        }) {
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
                    pid_start: pid_start(pid),
                    queued_at: senior,
                    last_poll: now,
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
            self.waiting.remove(idx);
            self.prune_seniority(now);
            return Ok((self.grant(req, wait_secs, clk, &mut events), events));
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
    /// answers softly (`released:false` + the reap reason): a cleanup
    /// path like `trap 'release $T' EXIT` should not hard-fail on a
    /// hold the daemon already took back.
    pub fn release(
        &mut self,
        token: &str,
        lane: &str,
        pid: u32,
        now: f64,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        let mut events = Vec::new();
        let reaped = self.reap_dead(now, &mut events);
        let Some(h) = self.held.iter().find(|h| h.token == token) else {
            if let Some(r) = reaped.iter().find(|r| r.token == token) {
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
        if h.lane != lane || h.pid != pid {
            return Err(Error::rejected(
                "Slot token is held by another caller — release must come \
                 from the holding lane and pid",
            ));
        }
        let idx = self.held.iter().position(|h| h.token == token).unwrap();
        let h = self.held.remove(idx);
        self.persist();
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
    /// `caller` sees its own lane's tokens; other lanes' holds show
    /// identity only — a token never leaves its lane's view.
    pub fn status(&mut self, caller: &str, now: f64) -> (Value, Vec<SlotEvent>) {
        let mut events = Vec::new();
        self.reap_dead(now, &mut events);
        (self.status_json(caller, now), events)
    }

    fn status_json(&self, caller: &str, now: f64) -> Value {
        let pool_json = |pool: Pool| {
            json!({
                "capacity": self.capacity(pool),
                "held": self.held.iter()
                    .filter(|h| h.kind.pool() == pool)
                    .map(|h| {
                        let mut j = json!({"kind": h.kind.as_str(),
                                    "lane": h.lane, "pid": h.pid,
                                    "age_secs": (now - h.acquired_at).max(0.0)});
                        if h.lane == caller {
                            j["token"] = json!(h.token);
                        }
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
        })
    }
}

/// Borrowed acquire inputs — `grant`/`find_hold` take them as one
/// argument.
struct SlotReq<'a> {
    kind: SlotKind,
    lane: &'a str,
    pid: u32,
    request_id: &'a str,
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
            s.status("dev-1", 0.0).0["pools"]["build"]["held"]
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
        let (status, _) = s.status("dev-1", 2.0);
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

    /// status shows tokens only to their own lane — other lanes see
    /// holder identity, never the token.
    #[test]
    fn status_hides_foreign_tokens() {
        let mut s = slots(2, 1, 900, &[]);
        let other = Child::spawn();
        let a = acquire_pid(&mut s, SlotKind::Build, "dev-1", other.pid(), "r1", 0.0);
        let b = acquire(&mut s, SlotKind::Build, "dev-2", "r2", 0.0);
        let t1 = a["token"].as_str().unwrap();
        let t2 = b["token"].as_str().unwrap();
        // dev-1 sees its own token, never dev-2's.
        let (s1, _) = s.status("dev-1", 0.0);
        let held = s1["pools"]["build"]["held"].as_array().unwrap();
        let mine = held.iter().find(|h| h["lane"] == "dev-1").unwrap();
        let theirs = held.iter().find(|h| h["lane"] == "dev-2").unwrap();
        assert_eq!(mine["token"], t1);
        assert!(
            theirs.get("token").is_none(),
            "foreign token hidden: {theirs}"
        );
        // An unaffiliated caller sees no tokens at all.
        let (s2, _) = s.status("unknown", 0.0);
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
        assert!(s.status("dev-2", 1.0).0["waiting"]
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
