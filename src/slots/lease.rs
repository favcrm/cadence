//! Slot lease/claim state machine: grant, acquire, release and status.

use super::{
    hold_and_wait_refusal, is_unregistered_lane, pid_start, wait_and_hold_refusal, Enrollment,
    Liveness, Pool, SlotCaller, SlotClock, SlotEvent, SlotHold, SlotKind, SlotReq, SlotWait, Slots,
    StrictBind, StrictCaller, WaitReason, MAX_WAITERS, MAX_WAITERS_PER_LANE,
};
use crate::error::{Error, Result};
use serde_json::{json, Value};
use uuid::Uuid;

impl Slots {
    /// A hold's identity is (request_id, pid, lane, kind): only the
    /// exact same caller re-polling adopts the grant — another
    /// process sharing a natural request id is a different caller.
    pub(super) fn find_hold(&self, req: &SlotReq<'_>) -> Option<&SlotHold> {
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
    pub(super) fn grant(
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

    /// Cross-pool deadlock guard: a caller may never QUEUE while its
    /// process already holds a slot — the CAD-113 other-pool cycle
    /// (A holds build and waits on suite while B holds suite and
    /// waits on build) and, under CAD-1268's shared budget, the
    /// same-pool cycle too (each holder waiting on a seat the other
    /// occupies). The guard keys on `(lane, pid)` — the holding
    /// *process*: two unrelated shells sharing a lane name (e.g.
    /// `$USER` when no `CADENCE_ALIAS` is set) never block each
    /// other. Granting without waiting is always fine: a caller that
    /// never waits holds no wait-edge, and a re-poll of the hold it
    /// already owns returns the same token before this runs.
    pub(super) fn holds_any_slot(&self, lane: &str, pid: u32) -> Option<Pool> {
        self.held
            .iter()
            .find(|h| h.lane == lane && h.pid == pid)
            .map(|h| h.kind.pool())
    }

    /// The mirror edge (CAD-1268): while one of a caller's requests
    /// is queued, that caller may not obtain a NEW grant — under the
    /// shared budget the holder it would become is itself a waiter,
    /// the same hold+wait deadlock `holds_any_slot` forbids, reached
    /// through the immediate-grant door. One process therefore keeps
    /// at most ONE request in the queue at a time: a second request
    /// identity from the same (lane, pid) is refused at enqueue time
    /// (and at probe/grant time), because granting one waiter while
    /// siblings still queue would mint exactly the forbidden
    /// holder-that-waits — and refusing the grant itself would leave
    /// every sibling eligible-but-refused forever.
    ///
    /// The match key is (lane, pid) — the holding process, the same
    /// key `holds_any_slot` uses on the held side. Strict callers
    /// bind the same physical process by construction (a strict
    /// waiter's pid is its verified holder), so enrollment is not
    /// part of the caller key: two waiters of one process under
    /// different enrollments are still one waiting caller.
    /// Re-polling the ALREADY-QUEUED request never reaches this —
    /// callers only ask here for a request identity that matched
    /// nothing in the queue.
    pub(super) fn waits_for_slot(&self, req: &SlotReq<'_>) -> Option<Pool> {
        self.waiting
            .iter()
            .find(|w| w.lane == req.lane && w.pid == req.pid)
            .map(|w| w.kind.pool())
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
    pub(super) fn acquire_as(
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
            if let Some(idx) = self.waiting.iter().position(&same) {
                let ranks = self.ranks(now);
                return Ok((
                    json!({"granted": false, "position": self.outranked(&ranks, idx) + 1,
                           "wait_secs": (now - self.waiting[idx].queued_at).max(0.0),
                           "held": self.held_in(pool),
                           "capacity": self.capacity(pool),
                           "total_held": self.held_total(),
                           "total_capacity": self.total_capacity()}),
                    events,
                ));
            }
            // A fresh request identity while this caller already has
            // a request queued: refused outright — it could never
            // join (one caller, one queued request) and a grant would
            // mint a holder that still waits.
            if let Some(waiting) = self.waits_for_slot(&req) {
                return Err(wait_and_hold_refusal(pool, waiting));
            }
            // A fresh probe grants exactly when an enqueue would —
            // per-pool AND aggregate capacity free and no eligible
            // waiter ahead — but never joins the queue and never
            // touches seniority (`peek_stamp` only reads).
            // A fresh probe grants when it leads AND the floors hold —
            // below floor it reports the resource reason, not a grant.
            let (floors, resource) = self.pool_floors(kind);
            let senior = self.peek_stamp(lane, kind, now);
            let w = SlotWait {
                request_id: request_id.to_string(),
                kind,
                lane: lane.to_string(),
                pid,
                pid_start: req.pid_start(),
                queued_at: senior,
                last_poll: now,
                wait_reason: resource.unwrap_or(WaitReason::Capacity),
                strict: req.strict.clone(),
            };
            self.waiting.push(w);
            let ranks = self.ranks(now);
            let idx = self.waiting.len() - 1;
            let next =
                self.grantable(kind, resource) && self.grant_outranked(&ranks, idx, &floors) == 0;
            let position = self.outranked(&ranks, idx) + 1;
            self.waiting.pop();
            self.prune_seniority(now);
            if next {
                return Ok((self.grant(req, 0.0, clk, &mut events)?, events));
            }
            if let Some(held) = self.holds_any_slot(lane, pid) {
                return Err(hold_and_wait_refusal(held, pool));
            }
            return Ok((
                json!({"granted": false, "position": position,
                       "wait_reason": resource.unwrap_or(WaitReason::Capacity).as_str(),
                       "held": self.held_in(pool),
                       "capacity": self.capacity(pool),
                       "total_held": self.held_total(),
                       "total_capacity": self.total_capacity()}),
                events,
            ));
        }
        let (idx, created) = match self.waiting.iter().position(&same) {
            Some(i) => (i, false),
            None => {
                // One caller, one queued request: a NEW request
                // identity from a process already waiting is refused
                // BEFORE it joins — the wait-then-hold mirror of
                // `holds_any_slot`. The already-queued request keeps
                // its exact place and repolls normally; nothing is
                // cancelled and nothing mutates.
                if let Some(waiting) = self.waits_for_slot(&req) {
                    return Err(wait_and_hold_refusal(pool, waiting));
                }
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
                    wait_reason: WaitReason::Capacity,
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
        // A grant needs per-pool AND aggregate capacity plus the
        // resource floors — a host low on memory or disk, or a budget
        // already fully granted, holds the caller in the queue with a
        // reason, never refuses it for being low.
        let (floors, resource) = self.pool_floors(kind);
        // Refresh the waiter's reason each poll — a grant that ran
        // into a floor updates it to memory/disk; a resource that
        // recovered flips it back to capacity.
        self.waiting[idx].wait_reason = resource.unwrap_or(WaitReason::Capacity);
        if self.grantable(kind, resource) && self.grant_outranked(&ranks, idx, &floors) == 0 {
            let wait_secs = (now - self.waiting[idx].queued_at).max(0.0);
            // Grant first: a strict grant whose write fails leaves the
            // caller queued exactly where it was.
            let granted = self.grant(req, wait_secs, clk, &mut events)?;
            self.waiting.remove(idx);
            self.prune_seniority(now);
            return Ok((granted, events));
        }
        // It must wait — but a process already holding a slot may not
        // queue at all (the hold-and-wait deadlock guard — CAD-1268
        // widened it to same-pool holds under the shared budget). The
        // forbidden waiter is removed, never registered.
        if let Some(held) = self.holds_any_slot(lane, pid) {
            self.waiting.remove(idx);
            self.prune_seniority(now);
            return Err(hold_and_wait_refusal(held, pool));
        }
        let position = self.outranked(&ranks, idx) + 1;
        let wait_secs = (now - self.waiting[idx].queued_at).max(0.0);
        let reason = resource.unwrap_or(WaitReason::Capacity).as_str();
        if created {
            // First poll and already queueing — tell observers the
            // wait started (re-polls don't repeat the event).
            events.push((
                lane.to_string(),
                "slot_waited",
                json!({"request_id": request_id, "kind": kind.as_str(),
                       "pool": pool.as_str(), "pid": pid,
                       "wait_reason": reason}),
            ));
        }
        Ok((
            json!({"granted": false, "position": position,
                   "wait_secs": wait_secs, "wait_reason": reason,
                   "held": self.held_in(pool), "capacity": self.capacity(pool),
                   "total_held": self.held_total(),
                   "total_capacity": self.total_capacity()}),
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
    pub(super) fn release_as(
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

    pub(super) fn status_json(&self, caller: SlotCaller<'_>, now: f64) -> Value {
        let pool_json = |pool: Pool| {
            json!({
                "capacity": self.capacity(pool),
                "held": self.held.iter()
                    .filter(|h| h.kind.pool() == pool)
                    .map(|h| {
                        let mut j = json!({"kind": h.kind.as_str(),
                                    "lane": h.lane, "pid": h.pid,
                                    "unregistered": is_unregistered_lane(&h.lane),
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
                "check": pool_json(Pool::Check),
            },
            "waiting": order.iter().map(|i| {
                let w = &self.waiting[*i];
                let (rank, _) = ranks[*i];
                json!({"request_id": w.request_id, "kind": w.kind.as_str(),
                       "lane": w.lane, "pid": w.pid,
                       "wait_secs": (now - w.queued_at).max(0.0),
                       "wait_reason": w.wait_reason.as_str(),
                       "starved": rank == 0, "priority": rank == 1,
                       "unregistered": is_unregistered_lane(&w.lane)})
            }).collect::<Vec<_>>(),
            "total_capacity": self.total_capacity(),
            "total_held": self.held_total(),
            "config": {
                "build_slots": self.config.build_slots,
                "suite_slots": self.config.suite_slots,
                "check_slots": self.config.check_slots,
                "total_slots": self.config.total_slots,
                "jobs_per_lane": self.config.jobs_per_lane,
                "starve_secs": self.config.starve_secs,
                "priority_lanes": self.config.priority_lanes,
                "max_hold_secs": self.config.max_hold_secs,
                "slot_mem_min_available_bytes": self.config.slot_mem_min_available_bytes,
                "slot_mem_min_available_check_bytes": self.config.slot_mem_min_available_check_bytes,
                "slot_disk_min_free_bytes": self.config.slot_disk_min_free_bytes,
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
    pub(super) fn accounting(&self, h: &SlotHold, now: f64) -> &'static str {
        if now - h.acquired_at >= self.config.max_hold_secs as f64 {
            "expired_pending_reconcile"
        } else {
            "held"
        }
    }

    /// Status fields of a hold's binding: `legacy`, or `strict` with
    /// its enrollment's authorization kept separate from the hold's own
    /// liveness and accounting, plus `reconcile_required` / `remedy`.
    pub(super) fn strict_hold_json(&self, h: &SlotHold, now: f64, j: &mut Value) {
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
