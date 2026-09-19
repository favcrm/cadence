//! Host build slots (CAD-113) — bounded, fair, observable cargo
//! build/test scheduling, owned by the daemon.
//!
//! Two pools share one queue: `build`/`test` requests draw on
//! `build_slots` (default 3), `suite` requests on `suite_slots`
//! (default 1) — independent, so a queued full suite never starves
//! ordinary builds. Grant order is FIFO with two modifiers:
//! `test`/`suite` requests from a configured priority lane outrank
//! everything ordinary, and anything waiting longer than
//! `starve_secs` (default 900) jumps to the front so priority can
//! never starve a lane out.
//!
//! A slot is held by token bound to a pid: `release` returns it, and
//! a holder whose pid dies is reaped on the next acquire/status so a
//! killed agent frees its slot. Waiting is client-side — `acquire`
//! answers instantly with granted-or-queued, and the caller polls
//! with a stable `request_id` so its place in line is sticky.

use serde_json::{json, Value};

use crate::error::{Error, Result};

/// `[host]` slot configuration from pm.yaml — every key optional,
/// unset keys keep the defaults.
#[derive(Clone, Debug)]
pub struct SlotConfig {
    /// Concurrent `build`+`test` grants (default 3).
    pub build_slots: usize,
    /// Concurrent `suite` grants (default 1).
    pub suite_slots: usize,
    /// `CARGO_BUILD_JOBS` value dispatch injects (default 4).
    pub jobs_per_lane: usize,
    /// Waiters older than this outrank even priority lanes
    /// (default 900) — the never-starve bound.
    pub starve_secs: u64,
    /// Lanes whose `test`/`suite` requests outrank ordinary requests
    /// (`[host] priority_lanes` — the reviewer lane, e.g. `qa-1`).
    pub priority_lanes: Vec<String>,
}

impl Default for SlotConfig {
    fn default() -> Self {
        Self {
            build_slots: 3,
            suite_slots: 1,
            jobs_per_lane: 4,
            starve_secs: 900,
            priority_lanes: Vec::new(),
        }
    }
}

/// Slot kinds map to pools: `build`/`test` share `build_slots`,
/// `suite` owns `suite_slots`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

struct SlotWait {
    request_id: String,
    kind: SlotKind,
    lane: String,
    pid: u32,
    queued_at: f64,
    /// Last time this request polled — a caller that stops polling is
    /// abandoned and reaped after `WAITER_TTL_SECS`, so a fast-failed
    /// or timed-out CLI never jams the queue behind a dead lane's pid.
    last_poll: f64,
}

struct SlotHold {
    token: String,
    kind: SlotKind,
    lane: String,
    pid: u32,
    acquired_at: f64,
}

/// Borrowed acquire inputs — `grant` takes them as one argument.
struct SlotReq<'a> {
    kind: SlotKind,
    lane: &'a str,
    pid: u32,
    request_id: &'a str,
}

/// One event the caller should emit — `(alias, kind, payload)`; the
/// daemon routes them through `store.event_public`.
pub type SlotEvent = (String, &'static str, Value);

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

/// A waiter that has not re-polled within this window is abandoned —
/// ~120× the CLI's 250ms poll cadence, generous under load.
const WAITER_TTL_SECS: f64 = 30.0;

/// The slot registry — in-memory on purpose: a daemon restart reaps
/// every holder anyway (its pid checks fail or the tokens are
/// forgotten), which is the desired fail-closed semantics.
#[derive(Default)]
pub struct Slots {
    pub config: SlotConfig,
    waiting: Vec<SlotWait>,
    held: Vec<SlotHold>,
}

impl Slots {
    pub fn new(config: SlotConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    fn capacity(&self, pool: Pool) -> usize {
        match pool {
            Pool::Build => self.config.build_slots,
            Pool::Suite => self.config.suite_slots,
        }
    }

    fn held_in(&self, pool: Pool) -> usize {
        self.held.iter().filter(|h| h.kind.pool() == pool).count()
    }

    /// Grant-order rank: starved waiters first, then priority lanes on
    /// test/suite, then plain FIFO — the key is (rank, queued_at).
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

    /// Drop dead-pid holders and waiters. Holder deaths emit
    /// `slot_released` with the reap reason; a dead waiter held
    /// nothing, so it drops silently.
    fn reap_dead(&mut self, now: f64, events: &mut Vec<SlotEvent>) {
        let dead: Vec<String> = self
            .held
            .iter()
            .filter(|h| !pid_alive(h.pid))
            .map(|h| {
                events.push((
                    h.lane.clone(),
                    "slot_released",
                    json!({"token": h.token, "kind": h.kind.as_str(),
                           "held_secs": (now - h.acquired_at).max(0.0),
                           "reason": "holder died"}),
                ));
                h.token.clone()
            })
            .collect();
        self.held.retain(|h| !dead.contains(&h.token));
        // Waiters drop on either abandonment signal: a dead pid, or a
        // poll gone silent past the TTL (a fast-failed --wait-secs 0
        // caller's pid may still be alive in its parent — only the
        // silence proves it walked away).
        self.waiting
            .retain(|w| pid_alive(w.pid) && now - w.last_poll <= WAITER_TTL_SECS);
    }

    /// Whether `w` is next in its pool's effective order.
    fn is_next(&self, w: &SlotWait, now: f64) -> bool {
        let pool = w.kind.pool();
        let mine = self.rank(w, now);
        !self
            .waiting
            .iter()
            .filter(|o| o.kind.pool() == pool && o.request_id != w.request_id)
            .any(|o| self.rank(o, now) < mine)
    }

    /// `w`'s place in effective service order — how many same-pool
    /// waiters outrank it, plus one.
    fn position(&self, w: &SlotWait, now: f64) -> usize {
        let pool = w.kind.pool();
        let mine = self.rank(w, now);
        self.waiting
            .iter()
            .filter(|o| o.kind.pool() == pool && o.request_id != w.request_id)
            .filter(|o| self.rank(o, now) < mine)
            .count()
            + 1
    }

    /// The grant itself: hold registration + the `slot_acquired`
    /// event, shared by the probe and the queueing path.
    fn grant(
        &mut self,
        req: SlotReq<'_>,
        wait_secs: f64,
        now: f64,
        events: &mut Vec<SlotEvent>,
    ) -> Value {
        events.push((
            req.lane.to_string(),
            "slot_acquired",
            json!({"token": req.request_id, "kind": req.kind.as_str(),
                   "pool": req.kind.pool().as_str(), "wait_secs": wait_secs,
                   "pid": req.pid}),
        ));
        self.held.push(SlotHold {
            token: req.request_id.to_string(),
            kind: req.kind,
            lane: req.lane.to_string(),
            pid: req.pid,
            acquired_at: now,
        });
        json!({"granted": true, "token": req.request_id,
               "kind": req.kind.as_str(), "wait_secs": wait_secs})
    }

    /// Non-blocking acquire: grants a token when the pool has room and
    /// the request leads its effective order, else reports the queue
    /// position. `request_id` makes client polls sticky — a re-poll
    /// keeps the original `queued_at` and refreshes `last_poll` (the
    /// abandonment signal). `probe` answers the same question WITHOUT
    /// leaving a waiter behind — the `--wait-secs 0` fast-fail — so a
    /// probe that cannot grant only reports where it would stand.
    pub fn acquire(
        &mut self,
        kind: SlotKind,
        lane: &str,
        pid: u32,
        request_id: &str,
        probe: bool,
        now: f64,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        let mut events = Vec::new();
        if pid == 0 {
            return Err(Error::rejected(
                "Slot acquire needs the holder's pid (`--pid`, default: caller's parent)",
            ));
        }
        self.reap_dead(now, &mut events);
        // Idempotent re-poll: already granted → the same token again.
        if let Some(h) = self.held.iter().find(|h| h.token == request_id) {
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
            if let Some(o) = self
                .waiting
                .iter()
                .find(|o| o.request_id == request_id && o.kind.pool() == pool)
            {
                let position = self.position(o, now);
                return Ok((
                    json!({"granted": false, "position": position,
                           "wait_secs": (now - o.queued_at).max(0.0),
                           "held": self.held_in(pool),
                           "capacity": self.capacity(pool)}),
                    events,
                ));
            }
            // A fresh probe grants exactly when an enqueue would —
            // capacity free and the request next — but never joins
            // the queue.
            let w = SlotWait {
                request_id: request_id.to_string(),
                kind,
                lane: lane.to_string(),
                pid,
                queued_at: now,
                last_poll: now,
            };
            if self.held_in(pool) < self.capacity(pool) && self.is_next(&w, now) {
                return Ok((
                    self.grant(
                        SlotReq {
                            kind,
                            lane,
                            pid,
                            request_id,
                        },
                        0.0,
                        now,
                        &mut events,
                    ),
                    events,
                ));
            }
            return Ok((
                json!({"granted": false, "position": self.position(&w, now),
                       "held": self.held_in(pool),
                       "capacity": self.capacity(pool)}),
                events,
            ));
        }
        let mut created = false;
        let idx = match self.waiting.iter().position(|w| w.request_id == request_id) {
            Some(i) => i,
            None => {
                self.waiting.push(SlotWait {
                    request_id: request_id.to_string(),
                    kind,
                    lane: lane.to_string(),
                    pid,
                    queued_at: now,
                    last_poll: now,
                });
                created = true;
                self.waiting.len() - 1
            }
        };
        // Refresh identity a re-registered request may have changed,
        // and the liveness stamp every poll owes.
        self.waiting[idx].kind = kind;
        self.waiting[idx].lane = lane.to_string();
        self.waiting[idx].pid = pid;
        self.waiting[idx].last_poll = now;

        let pool = kind.pool();
        let w = &self.waiting[idx];
        if self.held_in(pool) < self.capacity(pool) && self.is_next(w, now) {
            let wait_secs = (now - w.queued_at).max(0.0);
            self.waiting.remove(idx);
            return Ok((
                self.grant(
                    SlotReq {
                        kind,
                        lane,
                        pid,
                        request_id,
                    },
                    wait_secs,
                    now,
                    &mut events,
                ),
                events,
            ));
        }
        let w = &self.waiting[idx];
        let position = self.position(w, now);
        let wait_secs = (now - w.queued_at).max(0.0);
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

    /// Return a held slot. Unknown tokens refuse — a typo must not
    /// silently pass.
    pub fn release(&mut self, token: &str, now: f64) -> Result<(Value, Vec<SlotEvent>)> {
        let mut events = Vec::new();
        self.reap_dead(now, &mut events);
        let Some(idx) = self.held.iter().position(|h| h.token == token) else {
            return Err(Error::rejected(format!(
                "Unknown slot token '{token}' — it was never granted or already released"
            )));
        };
        let h = self.held.remove(idx);
        events.push((
            h.lane.clone(),
            "slot_released",
            json!({"token": h.token, "kind": h.kind.as_str(),
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
    pub fn status(&mut self, now: f64) -> (Value, Vec<SlotEvent>) {
        let mut events = Vec::new();
        self.reap_dead(now, &mut events);
        (self.status_json(now), events)
    }

    fn status_json(&self, now: f64) -> Value {
        let pool_json = |pool: Pool| {
            json!({
                "capacity": self.capacity(pool),
                "held": self.held.iter()
                    .filter(|h| h.kind.pool() == pool)
                    .map(|h| json!({"token": h.token, "kind": h.kind.as_str(),
                                    "lane": h.lane, "pid": h.pid,
                                    "age_secs": (now - h.acquired_at).max(0.0)}))
                    .collect::<Vec<_>>(),
            })
        };
        let mut waiting: Vec<&SlotWait> = self.waiting.iter().collect();
        waiting.sort_by_key(|w| (self.rank(w, now).0, (w.queued_at * 1e6) as i64));
        json!({
            "pools": {
                "build": pool_json(Pool::Build),
                "suite": pool_json(Pool::Suite),
            },
            "waiting": waiting.iter().map(|w| {
                let (rank, _) = self.rank(w, now);
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
            },
        })
    }
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

    fn acquire(s: &mut Slots, kind: SlotKind, lane: &str, req: &str, now: f64) -> Value {
        s.acquire(kind, lane, std::process::id(), req, false, now)
            .unwrap()
            .0
    }

    fn probe(s: &mut Slots, kind: SlotKind, lane: &str, req: &str, now: f64) -> Value {
        s.acquire(kind, lane, std::process::id(), req, true, now)
            .unwrap()
            .0
    }

    #[test]
    fn fifo_within_a_pool() {
        let mut s = slots(1, 1, 900, &[]);
        let a = acquire(&mut s, SlotKind::Build, "dev-1", "r1", 100.0);
        assert_eq!(a["granted"], true);
        let b = acquire(&mut s, SlotKind::Build, "dev-2", "r2", 100.0);
        assert_eq!(b["granted"], false);
        s.release("r1", 110.0).unwrap();
        let b = acquire(&mut s, SlotKind::Build, "dev-2", "r2", 110.0);
        assert_eq!(b["granted"], true);
        // Re-poll is idempotent — same token back.
        let again = acquire(&mut s, SlotKind::Build, "dev-2", "r2", 111.0);
        assert_eq!(again["token"], "r2");
    }

    #[test]
    fn priority_lane_outranks_but_starve_wins() {
        let mut s = slots(1, 1, 60, &["qa-1"]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 1.0);
        // Reviewer test arrives later but outranks the build waiter.
        acquire(&mut s, SlotKind::Test, "qa-1", "w2", 2.0);
        s.release("h1", 3.0).unwrap();
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
        s.release("h1", 71.0).unwrap();
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
        let t = acquire(&mut s, SlotKind::Test, "qa-1", "t1", 0.0);
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
        assert_eq!(s.status(1.0).0["waiting"].as_array().unwrap().len(), 0);
        // A probe on a free pool grants like an acquire.
        s.release("h1", 1.0).unwrap();
        let p = probe(&mut s, SlotKind::Build, "dev-2", "q1", 1.0);
        assert_eq!(p["granted"], true);
        assert_eq!(p["token"], "q1");
    }

    #[test]
    fn abandoned_waiter_is_reaped() {
        let mut s = slots(1, 1, 900, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        // w1 queues at t=0 and never polls again — live pid, stale.
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 0.0);
        // w2 queues at t=10 keeping its poll fresh.
        acquire(&mut s, SlotKind::Build, "dev-3", "w2", 10.0);
        s.release("h1", 20.0).unwrap();
        // At t=31 w1's poll is 31s stale (> TTL) — reaped; w2 granted.
        let g = acquire(&mut s, SlotKind::Build, "dev-3", "w2", 31.0);
        assert_eq!(g["granted"], true, "stale waiter must not outrank");
        // Inside the TTL w1 still holds its place.
        let mut s = slots(1, 1, 900, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 0.0);
        acquire(&mut s, SlotKind::Build, "dev-3", "w2", 1.0);
        s.release("h1", 2.0).unwrap();
        let g = acquire(&mut s, SlotKind::Build, "dev-3", "w2", 2.0);
        assert_eq!(g["granted"], false, "fresh waiter keeps its place");
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w1", 2.0);
        assert_eq!(g["granted"], true);
    }
}
