//! Slot heartbeat: reap dead/expired holders and waiters, expire enrollments.

use super::{
    pid_alive, pid_matches, AuthState, Liveness, Reaped, SlotEvent, SlotHold, Slots, RECENT_REAPED,
    WAITER_TTL_SECS,
};
use serde_json::json;

impl Slots {
    /// Drop dead/recycled/expired holders and dead or silent waiters.
    /// Holder drops emit `slot_released` with the reap reason (and no
    /// token — events never carry one); a dead waiter held nothing,
    /// so it drops silently. Returns what was reaped so `release`
    /// can answer a just-reaped token softly. Strict holds follow the
    /// tri-state rule instead ([`Self::reap_strict`]), and strict
    /// waiters are revalidated against their enrollment here — before
    /// any caller ranks the queue.
    pub(super) fn reap_dead(&mut self, now: f64, events: &mut Vec<SlotEvent>) -> Vec<Reaped> {
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
    pub(super) fn reap_strict(
        &mut self,
        now: f64,
        events: &mut Vec<SlotEvent>,
        dead: &mut Vec<Reaped>,
    ) {
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
    pub(super) fn expire_enrollments(&mut self, now: f64, events: &mut Vec<SlotEvent>) {
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
}
