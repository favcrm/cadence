//! CAD-534: `cadence daemon` slots RPC handlers — moved verbatim from src/daemon.rs; the
//! item→file map is src/daemon/split-map.toml
//! (scripts/split-daemon regenerates it).

use super::*;

use crate::slots::SlotKind;
use std::io::Write;

/// Wait until runner root `pid` (a child of this daemon, leader of its
/// own process group) has exited WITHOUT reaping it, then SIGKILL what
/// is left of its group. While the zombie is unreaped the kernel cannot
/// hand its pid — the group id — to another process, so the kill can
/// only reach the runner's own stragglers (CAD-230b).
fn end_process_group(pid: u32) {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            break;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            // Not our child any more (already reaped): the group id may
            // be reused — never signal it.
            return;
        }
    }
    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
}

/// How often the daemon re-reads strict holders (CAD-230b).
const SLOT_WATCH_TICK: Duration = Duration::from_secs(1);

/// A launched runner queues this long for its slot unless asked
/// otherwise (`wait_secs`), and never longer than the cap.
const RUNNER_WAIT_SECS: u64 = 600;

const RUNNER_MAX_WAIT_SECS: u64 = 86_400;

impl Shared {
    /// Slot lifecycle events ride the durable event stream addressed
    /// to the requesting lane — an agent sees why its build waited in
    /// its own `agent events` view.
    pub(super) fn emit_slot_events(&self, events: Vec<crate::slots::SlotEvent>) {
        if events.is_empty() {
            return;
        }
        for (lane, kind, payload) in events {
            let _ = self.store.event_public(&lane, kind, payload);
        }
        self.wake();
    }

    /// The slot caller's connection-bound identity. The peer's pid
    /// comes from `SO_PEERCRED` and its `/proc` ancestry is walked;
    /// the NEAREST identity node on that chain decides, so a caller's
    /// own pane or endpoint beats any outer one and resolution never
    /// depends on map order:
    ///
    /// - a registered pty pane → the legacy binding (CAD-113, unchanged):
    ///   `lane` is the pane's alias and every pid on the chain may bind
    ///   a hold (`acquire --pid $$` claims the invoking shell);
    /// - the root of a strict enrollment (CAD-230: a managed provider
    ///   the daemon launched) → the strict binding: the peer must be
    ///   that exact process or reach it through a complete ancestry
    ///   verified hop by hop (pid + starttime + uid), and only that
    ///   verified segment may bind a hold. A failed verification
    ///   refuses — it never falls through to an outer pane.
    ///
    /// A pane row matches only while its recorded process start time
    /// does (CAD-385, [`crate::peer::AgentPids`]): a row whose pid was
    /// reused is no pane at all — the caller falls through exactly as
    /// an unregistered process — and a row with no recorded start on
    /// the chain refuses the call, naming the remedy.
    ///
    /// Fail-closed: an unreadable ancestry or no match refuses the
    /// call — there is no `operator` fallback; a caller detached from
    /// every pane and endpoint holds no lane at all. `Ok(None)` is the
    /// clean "no identity" answer — for [`Self::rpc_slot_reconcile`] a
    /// precondition of operator authority, never proof of it.
    pub(super) fn slot_identity(&self, peer_pid: u32) -> Result<Option<SlotWho>> {
        // CAD-482: an asserted caller is exactly what the test named —
        // this process's ambient ancestry is never consulted under a
        // seam scope. Without the feature `asserted()` is `None` and
        // this consult compiles out.
        if let Some(asserted) = crate::test_seam::asserted() {
            return Ok(match asserted {
                crate::test_seam::Asserted::Agent(lane) => Some(SlotWho::Pane {
                    lane,
                    chain: vec![peer_pid],
                }),
                _ => None,
            });
        }
        let chain = adapter::pty::caller_chain(peer_pid).ok_or_else(|| {
            Error::rejected(format!(
                "Slot caller pid {peer_pid}: /proc ancestry unreadable — \
                 caller identity underivable"
            ))
        })?;
        let recorded =
            crate::peer::AgentPids::classify(self.store.pty_pane_pids().unwrap_or_default());
        recorded
            .refuse_unproven_on(&chain)
            .map_err(|why| Error::rejected(format!("Slot caller pid {peer_pid}: {why}")))?;
        let panes = recorded.live();
        let pane_at = chain.iter().position(|pid| panes.contains_key(pid));
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let root_at = slots.nearest_enrolled_root(&chain);
        match (pane_at, root_at) {
            (Some(p), Some(r)) if p == r => Err(Error::rejected(format!(
                "Slot caller pid {peer_pid}: pid {} is both a registered pane and \
                 an enrolled endpoint — caller identity ambiguous",
                chain[p]
            ))),
            (pane, Some(r)) if pane.is_none_or(|p| r < p) => Ok(Some(SlotWho::Strict(
                slots.strict_caller(peer_pid, chain[r])?,
            ))),
            (Some(p), _) => {
                let lane = adapter::pty::nearest_pane(&chain[p..], &panes)
                    .cloned()
                    .unwrap_or_default();
                Ok(Some(SlotWho::Pane { lane, chain }))
            }
            _ => Ok(None),
        }
    }

    /// [`Self::slot_identity`] for the slot verbs: no identity refuses.
    fn slot_caller(&self, peer_pid: u32) -> Result<SlotWho> {
        self.slot_identity(peer_pid)?.ok_or_else(|| {
            Error::rejected(format!(
                "Slot caller pid {peer_pid} descends from no registered \
                 pane and no enrolled managed endpoint — caller identity \
                 underivable"
            ))
        })
    }

    /// The pid a slot request may bind: the socket peer itself or one
    /// of its /proc ancestors — anything else is a foreign pid and the
    /// request is refused, not rebound. `pid` absent means the peer.
    fn claimed_slot_pid(params: &Value, chain: &[u32], peer_pid: u32) -> Result<u32> {
        let pid = optional_u64(params, "pid")
            .map(|p| p as u32)
            .unwrap_or(peer_pid);
        if pid == 0 || !chain.contains(&pid) {
            return Err(Error::rejected(format!(
                "Slot caller pid {peer_pid} cannot claim pid {pid} — it is \
                 not the connection peer or one of its ancestors"
            )));
        }
        Ok(pid)
    }

    /// Non-blocking slot acquire (CAD-113) — the caller polls with a
    /// stable `request_id`; each answer is granted-or-queue-position.
    /// `lane`/`pid` are never taken from the request: identity is the
    /// connection's, and a `pid` claim off the peer's own ancestry (or,
    /// for a strict caller, off its verified segment) is refused.
    pub(super) fn rpc_slot_acquire(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.revalidate_enrollments()?;
        let who = self.slot_caller(peer_pid)?;
        let kind = SlotKind::parse(required_str(params, "kind")?)?;
        let request_id = required_str(params, "request_id")?;
        if request_id.len() > 128 {
            return Err(Error::rejected("Slot request_id must be <= 128 bytes"));
        }
        let pid = Self::claimed_slot_pid(params, who.chain(), peer_pid)?;
        // `probe` is the read-only fast-fail: it answers granted or
        // position without leaving a waiter in the queue.
        let probe = params["probe"].as_bool().unwrap_or(false);
        // `exec` (CAD-230b, `build-slot run`): the requesting peer IS the
        // process that execs into the command, so it must claim itself —
        // never an ancestor. A strict caller's hold is then exec-bound;
        // a pane caller's legacy hold is otherwise unchanged.
        let exec = params["exec"].as_bool().unwrap_or(false);
        if exec && pid != peer_pid {
            return Err(crate::slots::exec_not_peer(pid));
        }
        let clk = crate::slots::SlotClock::at((self.slot_clock)(), epoch_secs());
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let (result, events) = match &who {
            SlotWho::Pane { lane, .. } => slots.acquire(kind, lane, pid, request_id, probe, clk)?,
            SlotWho::Strict(caller) => {
                slots.acquire_strict_bound(kind, caller, pid, request_id, probe, clk, exec)?
            }
        };
        drop(slots);
        self.emit_slot_events(events);
        Ok(result)
    }

    /// `slot_release` — the release must name the holding (lane,
    /// pid): a token alone is not authority to free another
    /// caller's slot. Both come from the connection: the lane is the
    /// peer's derived pane (or enrollment owner) and the pid must be on
    /// the peer's own ancestry, so a caller can only ever name its own
    /// lineage.
    pub(super) fn rpc_slot_release(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.revalidate_enrollments()?;
        let who = self.slot_caller(peer_pid)?;
        let token = required_str(params, "token")?;
        let pid = Self::claimed_slot_pid(params, who.chain(), peer_pid)?;
        let now = (self.slot_clock)();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let (result, events) = match &who {
            SlotWho::Pane { lane, .. } => slots.release(token, lane, pid, now)?,
            SlotWho::Strict(caller) => slots.release_strict(token, caller, pid, now)?,
        };
        drop(slots);
        self.emit_slot_events(events);
        Ok(result)
    }

    /// `slot_status` — the pools and queue are public, but a hold's
    /// token shows only to its owner: the caller whose derived lane
    /// matches the hold and whose own ancestry includes the hold's
    /// pid. A `lane` param is ignored — identity is the connection's.
    pub(super) fn rpc_slot_status(&self, _params: &Value, peer_pid: u32) -> Result<Value> {
        self.revalidate_enrollments()?;
        let who = self.slot_caller(peer_pid)?;
        let (status, events) = self.slots.lock().unwrap_or_else(|e| e.into_inner()).status(
            crate::slots::SlotCaller {
                lane: who.lane(),
                pids: who.chain(),
            },
            (self.slot_clock)(),
        );
        self.emit_slot_events(events);
        Ok(status)
    }

    /// `slot_reconcile` — the one mutating operator path over a strict
    /// hold (CAD-230). Operator authority is the connection's: a caller
    /// that derives ANY slot identity (a pane or an enrolled endpoint)
    /// is an agent and is refused, and so is one that is not PROVABLY
    /// the operator ([`Self::proven_operator`], CAD-276);
    /// identity-shaped request fields are refused rather than read. The
    /// daemon frees the hold only on its own proof of the holder's
    /// death; see [`Slots::reconcile`].
    pub(super) fn rpc_slot_reconcile(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        for field in ["by", "operator", "actor", "alias", "lane", "pid"] {
            if params.get(field).is_some() {
                return Err(Error::rejected(format!(
                    "slot reconcile authority is connection-bound; request field \
                     '{field}' is not accepted"
                )));
            }
        }
        if let Some(who) = self.slot_identity(peer_pid)? {
            return Err(Error::rejected(format!(
                "slot reconcile is an operator action — this connection is agent \
                 '{}'; run it outside every pane and managed endpoint",
                who.lane()
            )));
        }
        self.proven_operator("slot reconcile", peer_pid)?;
        let enrollment = required_str(params, "enrollment_id")?;
        let token = required_str(params, "token")?;
        let evidence = params
            .get("evidence")
            .filter(|e| e.is_object())
            .ok_or_else(|| Error::rejected("slot reconcile needs an evidence object"))?;
        let (result, events) = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reconcile(enrollment, token, evidence, (self.slot_clock)())?;
        self.emit_slot_events(events);
        Ok(result)
    }

    /// Who may ask the daemon to launch a runner (CAD-230b), from the
    /// connection alone: a pane agent (the legacy derivation), an ACTIVE
    /// enrolled managed endpoint (phase a), or — deriving neither — the
    /// proven operator ([`crate::peer::operator_proof`]). A runner's own
    /// process tree, a revoked or expired endpoint, a failed strict
    /// verification and anything unproven are refused, naming the rule.
    fn launch_requester(&self, peer_pid: u32) -> Result<crate::runner::Requester> {
        let requester = |kind: &str, lane: String| crate::runner::Requester {
            kind: kind.to_string(),
            lane,
        };
        match self.slot_identity(peer_pid)? {
            Some(SlotWho::Pane { lane, .. }) => Ok(requester("pane", lane)),
            Some(SlotWho::Strict(caller)) => {
                let lane = self
                    .slots
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .launch_lane(&caller, (self.slot_clock)())?;
                Ok(requester("managed", lane))
            }
            // `(operator)` can never be an agent alias, so the operator's
            // runners never share a lane (or its events) with an agent.
            None => match self.operator_evidence(peer_pid) {
                Ok(()) => Ok(requester("operator", OPERATOR_LANE.to_string())),
                Err(why) => Err(Error::rejected(format!(
                    "build-slot launch needs a pane agent, an enrolled managed \
                     endpoint or the proven operator — this connection derives no \
                     slot identity and is not provably the operator: {why}. Launch \
                     from an agent's pane or managed endpoint, or from an attached \
                     operator shell"
                ))),
            },
        }
    }

    /// `slot_launch` (CAD-230b) — run one of a project's recipes as a
    /// daemon-launched runner. The request names only the recipe, the
    /// project, optionally a checkout of one of its registered repos and
    /// how long to queue; anything command- or identity-shaped is
    /// refused. The daemon resolves the launch intent from project
    /// config, writes its digest bound to a fresh runner id BEFORE
    /// spawning, spawns the gated process (its own process group, output
    /// to `<state>/runners/<id>.log`), enrolls it as root = worker and
    /// hands the queue wait, the grant, the gate and the exit receipt to
    /// a runner thread. Answers at once with the runner id; the CLI
    /// polls `slot_runner`.
    pub(super) fn rpc_slot_launch(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        const FIELDS: [&str; 4] = ["recipe", "project", "worktree", "wait_secs"];
        if let Some(extra) = params
            .as_object()
            .and_then(|o| o.keys().find(|k| !FIELDS.contains(&k.as_str())))
        {
            return Err(Error::rejected(format!(
                "build-slot launch takes only recipe, project, worktree and \
                 wait_secs — '{extra}' is refused: a recipe's argv, cwd and env \
                 come only from project config, and who is asking only from the \
                 connection"
            )));
        }
        self.revalidate_enrollments()?;
        let requester = self.launch_requester(peer_pid)?;
        let recipe = required_str(params, "recipe")?;
        let project = required_str(params, "project")?;
        let worktree = optional_text(params, "worktree")?.map(Path::new);
        let wait_secs = match params.get("wait_secs") {
            None | Some(Value::Null) => RUNNER_WAIT_SECS,
            Some(v) => v
                .as_u64()
                .ok_or_else(|| Error::rejected("wait_secs must be a whole number of seconds"))?
                .min(RUNNER_MAX_WAIT_SECS),
        };
        let intent = crate::runner::resolve(&self.pm_dir()?, project, recipe, worktree)?;
        let log = crate::runner::log_path(&self.state_dir, &intent.runner_id);
        let mut receipt =
            crate::runner::Receipt::pending(&intent, requester.clone(), &log, epoch_secs());
        // The digest is bound to the runner id durably before anything
        // is spawned.
        crate::runner::write_receipt(&self.state_dir, &receipt)?;
        let env: Vec<(String, String)> = intent
            .env
            .iter()
            .filter_map(|name| self.provider_env.var(name).map(|v| (name.clone(), v)))
            .collect();
        let child = match crate::runner::spawn_gated(&intent, &env, &log) {
            Ok(child) => child,
            Err(e) => {
                receipt.finish("refused", Some(e.to_string()), epoch_secs());
                let _ = crate::runner::write_receipt(&self.state_dir, &receipt);
                return Err(e);
            }
        };
        // The gate holds the recipe until the go line; any refusal from
        // here on closes it, so nothing ever runs unenrolled or unslotted.
        let refuse = |mut child: std::process::Child,
                      mut receipt: crate::runner::Receipt,
                      e: Error|
         -> Result<Value> {
            drop(child.stdin.take());
            let _ = child.wait();
            receipt.finish("refused", Some(e.to_string()), epoch_secs());
            let _ = crate::runner::write_receipt(&self.state_dir, &receipt);
            Err(e)
        };
        let clk = crate::slots::SlotClock::at((self.slot_clock)(), epoch_secs());
        let enrolled = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enroll_runner(
                &requester.lane,
                &intent.runner_id,
                &intent.digest,
                child.id(),
                clk,
            );
        let (enrollment_id, root, events) = match enrolled {
            Ok(enrolled) => enrolled,
            Err(e) => return refuse(child, receipt, e),
        };
        self.emit_slot_events(events);
        receipt.state = "queued".into();
        receipt.pid = Some(root.pid);
        receipt.starttime = Some(root.starttime);
        receipt.enrollment_id = Some(enrollment_id.clone());
        if let Err(e) = crate::runner::write_receipt(&self.state_dir, &receipt) {
            let events = self
                .slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .end_runner(
                    &enrollment_id,
                    "runner receipt unwritable",
                    (self.slot_clock)(),
                );
            self.emit_slot_events(events);
            return refuse(child, receipt, e);
        }
        let answer = json!({
            "runner_id": intent.runner_id, "state": "queued",
            "project": intent.project, "recipe": intent.recipe,
            "kind": intent.kind.as_str(), "digest": intent.digest,
            "head_sha": intent.head_sha, "log_path": receipt.log_path,
            "lane": requester.lane, "requester": requester.kind,
        });
        let shared = Arc::clone(self);
        let kind = intent.kind;
        thread::spawn(move || shared.run_runner(child, receipt, kind, wait_secs));
        Ok(answer)
    }

    /// One runner's life after launch (CAD-230b): queue for its slot as
    /// the exact enrolled process, and only once granted record
    /// `running` and open the gate — a crash after that write reads as
    /// `unknown`, never as "never ran". Then wait for the exit (the
    /// daemon is the parent, so it reaps), record the exit receipt, let
    /// the tri-state reaper free the hold on the now-dead holder, and
    /// revoke the enrollment. A refusal, a queue timeout or a closing
    /// daemon closes the gate instead: the recipe never starts.
    fn run_runner(
        self: Arc<Self>,
        mut child: std::process::Child,
        mut receipt: crate::runner::Receipt,
        kind: SlotKind,
        wait_secs: u64,
    ) {
        let enrollment = receipt.enrollment_id.clone().unwrap_or_default();
        let deadline = Instant::now() + Duration::from_secs(wait_secs);
        let mut gate = child.stdin.take();
        let queued: std::result::Result<(), (&str, String)> = loop {
            if self.closing.load(Ordering::SeqCst) {
                break Err((
                    "cancelled",
                    "the daemon is shutting down — the gate never opened".into(),
                ));
            }
            let clk = crate::slots::SlotClock::at((self.slot_clock)(), epoch_secs());
            let polled = self
                .slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .acquire_runner(&enrollment, kind, clk);
            match polled {
                Ok((answer, events)) => {
                    self.emit_slot_events(events);
                    if answer["granted"].as_bool() == Some(true) {
                        break Ok(());
                    }
                }
                Err(e) => break Err(("refused", e.to_string())),
            }
            if let Ok(Some(status)) = child.try_wait() {
                break Err((
                    "refused",
                    format!("the runner process ended before its slot was granted ({status})"),
                ));
            }
            if Instant::now() >= deadline {
                break Err((
                    "timed_out",
                    format!(
                        "no {} slot within {wait_secs}s — the gate never opened",
                        kind.as_str()
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(250));
        };
        match queued {
            Err((state, why)) => {
                drop(gate.take());
                let _ = child.wait();
                receipt.finish(state, Some(why), epoch_secs());
            }
            Ok(()) => {
                // The digest names a source HEAD; a checkout that moved
                // while this runner queued is not that source.
                let head = crate::runner::head_of(Path::new(&receipt.worktree));
                if head.as_deref() != Some(receipt.head_sha.as_str()) {
                    drop(gate.take());
                    let _ = child.wait();
                    receipt.finish(
                        "refused",
                        Some(format!(
                            "the checkout's HEAD moved while queued ({} → {}) — the \
                             gate never opened; launch again to bind the new source",
                            receipt.head_sha,
                            head.as_deref().unwrap_or("unreadable")
                        )),
                        epoch_secs(),
                    );
                    return self.finish_runner(&enrollment, receipt);
                }
                // A closing daemon opens no gate, even on a grant that
                // raced its shutdown.
                if self.closing.load(Ordering::SeqCst) {
                    drop(gate.take());
                    let _ = child.wait();
                    return;
                }
                receipt.dirty |= crate::runner::is_dirty(Path::new(&receipt.worktree));
                receipt.state = "running".into();
                receipt.started = Some(epoch_secs());
                let opened = crate::runner::write_receipt(&self.state_dir, &receipt).is_ok()
                    && gate.as_mut().is_some_and(|g| {
                        g.write_all(crate::runner::go_line(&receipt.runner_id).as_bytes())
                            .and_then(|_| g.flush())
                            .is_ok()
                    });
                drop(gate.take());
                // The recipe's process group ends with it: once the root
                // has exited — observed WITHOUT reaping it, so its pid
                // (the group id) cannot be reused yet — any straggler
                // left in the group (a backgrounded job, the rustc of a
                // killed cargo) is killed, so nothing keeps building
                // outside the slot that is about to free.
                end_process_group(child.id());
                let status = child.wait();
                match (opened, status) {
                    (false, _) => receipt.finish(
                        "refused",
                        Some(
                            "the running receipt could not be written — the gate stayed closed"
                                .into(),
                        ),
                        epoch_secs(),
                    ),
                    (true, Ok(status)) => {
                        use std::os::unix::process::ExitStatusExt;
                        receipt.exit_code = status.code();
                        receipt.signal = status.signal();
                        receipt.finish("exited", None, epoch_secs());
                    }
                    (true, Err(e)) => receipt.finish(
                        "unknown",
                        Some(format!("waiting for the runner failed: {e}")),
                        epoch_secs(),
                    ),
                }
            }
        }
        self.finish_runner(&enrollment, receipt);
    }

    /// Close a runner: free its hold (proof of death only), revoke its
    /// enrollment, write the exit receipt and tell the requester's lane.
    fn finish_runner(&self, enrollment: &str, receipt: crate::runner::Receipt) {
        // A daemon that is closing owns no state any more — its successor
        // does, once the singleton frees. It writes nothing: the next boot
        // reports this runner `unknown` rather than race that daemon's
        // `slots.json` and receipt.
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        let reason = format!("runner {} {}", receipt.runner_id, receipt.state);
        let events = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .end_runner(enrollment, &reason, (self.slot_clock)());
        self.emit_slot_events(events);
        if let Err(e) = crate::runner::write_receipt(&self.state_dir, &receipt) {
            eprintln!(
                "runner {}: exit receipt write failed: {e}",
                receipt.runner_id
            );
        }
        let _ = self.store.event_public(
            &receipt.requester.lane,
            "runner_finished",
            json!({"runner_id": receipt.runner_id, "recipe": receipt.recipe,
                   "project": receipt.project, "state": receipt.state,
                   "exit_code": receipt.exit_code, "signal": receipt.signal,
                   "digest": receipt.digest, "head_sha": receipt.head_sha}),
        );
        self.wake();
    }

    /// `slot_runner` — one runner's receipt. Readable by whoever may ask
    /// about slots at all: any connection with a slot identity, or the
    /// proven operator. Receipts carry no credential (never a token).
    pub(super) fn rpc_slot_runner(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        if self.slot_identity(peer_pid)?.is_none() {
            self.proven_operator("slot runner", peer_pid)?;
        }
        let id = required_str(params, "runner_id")?;
        let receipt = crate::runner::read_receipt(&self.state_dir, id)?;
        serde_json::to_value(&receipt).map_err(|e| Error::internal(e.to_string()))
    }

    /// CAD-230b: a strict hold ends with its exact holder, observed by
    /// the daemon itself — no release call and no other client's slot
    /// call needed. Legacy holds keep their reap-on-call rule.
    pub(super) fn run_slot_watch(self: &Arc<Self>) {
        while !self.closing.load(Ordering::SeqCst) {
            let events = self
                .slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reap_strict_holds((self.slot_clock)());
            self.emit_slot_events(events);
            let deadline = Instant::now() + SLOT_WATCH_TICK;
            while !self.closing.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}
