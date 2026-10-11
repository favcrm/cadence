//! Queue ranking and seniority for host build slots.

use super::{is_unregistered_lane, SlotKind, SlotWait, Slots, WaitReason};

impl Slots {
    /// Grant-order rank: starved `(lane, kind)`s first, then priority
    /// lanes on test/suite, then plain FIFO — the key is
    /// (rank, queued_at). Stamps were clamped at ENQUEUE time
    /// (`stamp_for`), not here — clamping at compare time would
    /// collapse every starved waiter to the same key and let a
    /// 901-second waiter tie a two-hour one; raw stamps keep rank 0
    /// in true FIFO order.
    fn rank(&self, w: &SlotWait, now: f64) -> (u8, f64) {
        // CAD-1021 (D5): an unregistered caller queues strictly behind
        // every registered waiter — below starved, priority and
        // ordinary alike — so a registered waiter never loses its place
        // to a caller the daemon could not name.
        if is_unregistered_lane(&w.lane) {
            return (3, w.queued_at);
        }
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
    pub(super) fn ranks(&self, now: f64) -> Vec<(u8, f64)> {
        self.waiting.iter().map(|w| self.rank(w, now)).collect()
    }

    /// How many same-pool waiters outrank entry `idx` — the `position`
    /// the queue reports (same-pool ordering is unchanged by the
    /// aggregate cap).
    pub(super) fn outranked(&self, ranks: &[(u8, f64)], idx: usize) -> usize {
        let pool = self.waiting[idx].kind.pool();
        ranks
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != idx && self.waiting[*i].kind.pool() == pool)
            .filter(|(_, r)| **r < ranks[idx])
            .count()
    }

    /// CAD-1268: how many grant-eligible waiters outrank entry `idx`
    /// GLOBALLY. A waiter is eligible when its own pool has free
    /// capacity and its kind's resource floors hold — a front waiter
    /// behind a saturated pool cannot head-of-line block a rival
    /// pool's claim on the shared budget. Priority/starvation rank
    /// still decides which eligible waiter leads, and a queue of only
    /// same-pool waiters reduces to `outranked`.
    pub(super) fn grant_outranked(
        &self,
        ranks: &[(u8, f64)],
        idx: usize,
        floors: &[Option<WaitReason>; 3],
    ) -> usize {
        ranks
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                *i != idx && {
                    let w = &self.waiting[*i];
                    floors[w.kind.pool() as usize].is_none()
                        && self.held_in(w.kind.pool()) < self.capacity(w.kind.pool())
                }
            })
            .filter(|(_, r)| **r < ranks[idx])
            .count()
    }

    /// The resource floor per pool, probed once per call so ranking
    /// eligibility is evaluated against one consistent host reading.
    pub(super) fn pool_floors(
        &self,
        kind: SlotKind,
    ) -> ([Option<WaitReason>; 3], Option<WaitReason>) {
        let floors = [
            self.resource_floor(SlotKind::Build),
            self.resource_floor(SlotKind::Suite),
            self.resource_floor(SlotKind::Check),
        ];
        (floors, floors[kind.pool() as usize])
    }

    /// A `(lane, kind)` episode stays alive while the lane keeps a
    /// waiter of that kind queued — `seen` tracks its freshest
    /// `last_poll` — and for `WAITER_TTL_SECS` after the lane's last
    /// poll, so a caller re-queueing under a new request_id inside
    /// that window keeps the lane's place. Past the TTL the lane has
    /// gone quiet: the next request anchors fresh. (The OTHER half
    /// of the rule lives in `grant`: a served lane's record is
    /// cleared even when waiters remain.)
    pub(super) fn prune_seniority(&mut self, now: f64) {
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
                None => now - s.seen <= super::WAITER_TTL_SECS,
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
    pub(super) fn stamp_for(&mut self, lane: &str, kind: SlotKind, now: f64) -> f64 {
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
        self.seniority
            .insert(key, super::Seniority { anchor, seen: now });
        anchor
    }

    /// What `stamp_for` would answer, without touching the episode —
    /// a probe is read-only and must never arm an anchor for a lane
    /// that hasn't actually queued.
    pub(super) fn peek_stamp(&self, lane: &str, kind: SlotKind, now: f64) -> f64 {
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
}
