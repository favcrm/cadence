//! Strict endpoint enrollments, owner revalidation, peer-PID verification,
//! strict-slot admission/release, and operator reconciliation.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;

use serde_json::{json, Value};
use uuid::Uuid;

#[cfg(test)]
use super::ProcFs;
use super::{
    exec_not_peer, runner_tree_refused, AuthState, Enrollment, EnrollmentSnapshot, Liveness,
    ProcIdentity, SlotClock, SlotEvent, SlotHold, SlotKind, Slots, StrictBind, StrictCaller,
    ENROLLMENT_TTL_SECS,
};
use crate::error::{Error, Result};

/// Strict enrollments (CAD-230 phase a) — minted, renewed, revoked
/// and revalidated only by the daemon; a caller never names one.
impl Slots {
    /// Read strict identity from `root` instead of `/proc` — tests.
    #[cfg(test)]
    pub(super) fn use_proc(&mut self, root: &Path, uid: u32) {
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

    pub(super) fn revoke_where(
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
    pub(super) fn hold_enrollment_caller(
        &self,
        token: &str,
        caller: &StrictCaller,
    ) -> StrictCaller {
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
