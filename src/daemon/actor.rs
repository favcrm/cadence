//! Actor execution: reserve/start an alias, open its endpoint, serialize turns,
//! and retain ownership through outcome handling and final teardown.
//!
//! Shared control state and RPC/auth dispatch remain in the daemon root.

use super::{
    epoch_secs, fmt_duration, format_unknown_fence, gate_backoff, AgentCtl, ContextDelivery,
    Lifecycle, Notify, Shared, StallWatch, AUTO_RESUME_EVENT, AUTO_STOP_MARKER_KINDS,
    UNKNOWN_GENERIC_REASON,
};
use crate::adapter::{self, registry, AdapterHooks, ProviderAdapter, SettledPoll, TurnResult};
use crate::error::{Error, Result};
use crate::store::{self, Agent, Message, Take};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

impl Shared {
    /// Initial submissions and fresh-session retries use the same committed
    /// profile. A provider reset must not discard the actor's current identity.
    fn prompt_with_profile(&self, message: &Message, mut body: String) -> Result<String> {
        let context = if crate::master::is_master(&message.alias) {
            Some(self.master_profile_context()?)
        } else {
            self.member_profile_context(message)?
        };
        if let Some(context) = context {
            body.reserve(2 + context.len());
            body.push_str("\n\n");
            body.push_str(&context);
        }
        Ok(body)
    }

    /// Spawn the actor for `alias` unless it is already owned (running or
    /// stopping) or fenced by an unknown outcome.
    pub fn launch_actor(self: &Arc<Self>, alias: &str) -> Result<()> {
        let mut lc = self.lifecycle.lock().unwrap();
        if lc.owned(alias) {
            return Err(Error::rejected("Agent is still running or stopping"));
        }
        self.start_actor_locked(&mut lc, alias, false)?;
        Ok(())
    }

    /// Fence check, enable, state write, spawn and insert — all while the
    /// lifecycle lock is held, so a concurrent stop/resume cannot
    /// interleave. The map entry is the ownership record: it is inserted
    /// before the actor becomes visible and removed only by the actor
    /// itself after full termination, so a present entry always means
    /// "still owned". Returns false when the alias is fenced.
    pub(super) fn start_actor_locked(
        self: &Arc<Self>,
        lc: &mut Lifecycle,
        alias: &str,
        enable: bool,
    ) -> Result<bool> {
        if self.execution_mode.fenced() {
            return Err(crate::backup::portable_fence::fence_refusal("agent start"));
        }
        // CAD-1380: a bound runtime with a foreign or unsafe cwd refuses before
        // any enable/state write or spawn. Ordinary aliases are unchanged.
        self.assistant_start_guard(alias)?;
        // A mailbox never gets an actor — regardless of who asked.
        let a = self.store.agent(alias)?;
        if a.archived_at.is_some() {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is archived — restore it before starting"
            )));
        }
        if !registry::has_actor(&a.provider, &a.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is an inbox — it has no actor to start"
            )));
        }
        if self.store.has_unknown(alias)? {
            // One write: the fence lands with the runtime fields
            // cleared — no `attention` + live endpoint window.
            self.store.set_state_detached(
                alias,
                "attention",
                Some(&self.uncertain_fence_text(alias)),
            )?;
            let _ = self.store.event_public(
                alias,
                "attention",
                json!({"reason": "uncertain_turn_preserved"}),
            );
            self.wake();
            return Ok(false);
        }
        if crate::master::is_master(alias) {
            crate::home::validate_workspace_isolated(
                &self.upload_workspace_dir,
                &self.state_dir,
                &self.provider_env,
            )?;
        }
        if enable {
            self.store.set_enabled(alias, true)?;
        }
        self.store.set_agent_state(alias, "starting", None)?;
        let ctl = Arc::new(AgentCtl {
            adapter: Mutex::new(None),
            wake: Notify::new(),
            thread: Mutex::new(None),
            stall: Mutex::new(StallWatch::default()),
            cloud_held: AtomicBool::new(false),
        });
        let shared = Arc::clone(self);
        let owned = alias.to_string();
        let spawned = Arc::clone(&ctl);
        let handle = crate::thread_budget::spawn(move || shared.run_actor(&owned, spawned))
            .map_err(|e| Error::internal(format!("actor thread not started: {e}")))?;
        *ctl.thread.lock().unwrap() = Some(handle);
        lc.agents.insert(alias.to_string(), ctl);
        Ok(true)
    }

    /// The actor loop: own the adapter, serialize turns, preserve unknown
    /// outcomes, and stop cleanly on disable/shutdown.
    fn run_actor(self: &Arc<Self>, alias: &str, ctl: Arc<AgentCtl>) {
        let outcome = self.actor_inner(alias, &ctl);
        // Also settles candidates when adapter construction or another
        // pre-proof step fails before `open_adopted` can be reached.
        self.adoptions_settle(alias);
        // Cleanup always runs: release the adapter, clear ctl, final
        // state. Detach — never kill — on daemon shutdown and on any
        // error exit (a fence): owned endpoints like a tmux pane
        // outlive the controller so the operator can inspect the screen
        // a failure left behind and `agent resume` re-adopt it. Explicit
        // stops keep `close()`, except while a Devin cloud turn is held.
        // `detach` defaults to `close` for adapters that own their
        // provider process, so managed endpoints are still reaped on
        // every exit.
        if let Some(adapter) = ctl.adapter.lock().unwrap().take() {
            if self.closing.load(Ordering::SeqCst)
                || outcome.is_err()
                || ctl.cloud_held.load(Ordering::SeqCst)
            {
                adapter.detach();
            } else {
                adapter.close();
            }
        }
        // The endpoint is gone: its build-slot enrollment admits no
        // more work (holds stay accounted until released or dead).
        self.revoke_endpoint(alias, "endpoint closed");
        {
            let mut pending = self.pending.lock().unwrap();
            pending.retain(|_, req| req.alias != alias);
            // Uncollected brokered answers die with the actor too — a
            // `request_wait` still blocked sees the handle gone and
            // reports `closed` to its caller.
            self.answered.lock().unwrap().retain(|_, (a, _)| a != alias);
        }
        // CAD-250 N2: a nudge never outlives the actor it was aimed at —
        // whatever ended it (stop, fence, shutdown), queued nudges close
        // here with `nudge_cancelled`, never pasted into a later pane.
        let nudge_reason = if self.closing.load(Ordering::SeqCst) {
            "shutdown"
        } else if outcome.is_err() {
            "fence"
        } else {
            "stop"
        };
        let _ = self.store.cancel_nudges_for(alias, nudge_reason);
        self.wake();
        let closing = self.closing.load(Ordering::SeqCst);
        match outcome {
            Err(ref error) => {
                // When unreconciled unknowns outlive the actor, keep the
                // provider's own account. The returned error is often the
                // generic review sentence; restamping from that alone would
                // hide the reason the operator has to inspect.
                let reason = if self.store.has_unknown(alias).unwrap_or(false) {
                    format_unknown_fence(&self.preserved_unknown_detail(alias, &error.to_string()))
                } else {
                    Self::public_actor_fatal_reason(error)
                };
                // One write: `attention` must never be observable with
                // the dead actor's endpoint still attached.
                let _ = self
                    .store
                    .set_state_detached(alias, "attention", Some(&reason));
                let _ = self
                    .store
                    .event_public(alias, "attention", json!({"reason": reason}));
                // CAD-413: an auto-resume whose open never reached
                // `ready` is still the newest marker — name it.
                let marker = self.store.last_event_of(alias, AUTO_STOP_MARKER_KINDS);
                if marker.is_ok_and(|m| m.is_some_and(|e| e.kind == AUTO_RESUME_EVENT)) {
                    self.auto_resume_failed(alias, &reason);
                }
            }
            Ok(()) => {
                let agent = self.store.agent(alias);
                let enabled = agent.as_ref().map(|a| a.enabled).unwrap_or(false);
                let state = if !enabled {
                    "stopped"
                } else if closing {
                    "offline"
                } else {
                    "idle"
                };
                // One write: the terminal state lands together with the
                // cleared endpoint fields.
                let _ = self.store.set_state_detached(alias, state, None);
            }
        }
        // Release the alias only after cleanup and the final state write:
        // until this removal, lifecycle callers still see the agent owned.
        let mut lc = self.lifecycle.lock().unwrap();
        if lc.agents.get(alias).is_some_and(|c| Arc::ptr_eq(c, &ctl)) {
            lc.agents.remove(alias);
        }
        drop(lc);
        self.wake();
    }

    /// Open/adopt and enroll the endpoint before any queued turn can run.
    pub(super) fn open_actor_adapter(
        self: &Arc<Self>,
        alias: &str,
        ctl: &Arc<AgentCtl>,
        agent: &Agent,
    ) -> Result<Arc<dyn ProviderAdapter>> {
        let log_path = self.provider_log_dir.join(format!("{alias}.provider.log"));
        let shared = Arc::clone(self);
        let owned = alias.to_string();
        let hooks = AdapterHooks {
            on_event: Box::new(move |method, params| {
                shared.on_provider_event(&owned, method, params)
            }),
            on_request: {
                let shared = Arc::clone(self);
                let owned = alias.to_string();
                Box::new(move |request| shared.on_provider_request(&owned, request))
            },
        };
        let adapter = adapter::build(agent, hooks, &log_path, &self.provider_env, self.agent_uid)?;
        let adapter: Arc<dyn ProviderAdapter> = Arc::from(adapter);
        // Publish before `open` so stop/shutdown can force-close the
        // transport while initialization RPCs are still in flight.
        *ctl.adapter.lock().unwrap() = Some(Arc::clone(&adapter));
        // A failed open returns Err — run_actor's cleanup detaches on
        // any error, which still closes owned provider processes
        // (detach defaults to close) while leaving a pty pane visible
        // for inspection.
        //
        // Hot restart: a candidate the shutdown marker recorded is
        // opened in adopt mode — the pane is re-validated against the
        // record (same pid, same native session) and the recorded
        // generation is reused so the turn's token stays valid. A
        // refused adoption falls back to the plain fence for this
        // agent only: the message goes `unknown`, the actor exits into
        // `attention`, and `turn_adopt_refused` names the check that
        // failed.
        let adoption = self.store.take_adoption(alias);
        let identity = match &adoption {
            // One pane proof covers the whole list — every entry for
            // an alias shares the marker's generation/pane/session.
            Some(entries) => match adapter.open_adopted(agent, &entries[0]) {
                Ok(identity) => identity,
                Err(error) => {
                    let reason = error.to_string();
                    let _ = self
                        .store
                        .orphan_running(alias, &format!("hot-restart adoption refused: {reason}"));
                    for e in entries {
                        let _ = self.store.event_public(
                            alias,
                            "turn_adopt_refused",
                            json!({"message": e.message_id,
                                   "turn_id": e.turn_id,
                                   "reason": reason}),
                        );
                    }
                    self.adoptions_settle(alias);
                    return Err(error);
                }
            },
            None => adapter.open(agent)?,
        };
        // Record how the endpoint came up *before* set_identity makes
        // it visible — a resume report polling the agent row must find
        // the outcome already written.
        if let Some(attach) = identity.attach {
            self.open_attach
                .lock()
                .unwrap()
                .insert(alias.to_string(), attach);
        }
        match &adoption {
            Some(entries) => {
                self.store.set_identity_adopted_with_quota(
                    alias,
                    &identity,
                    entries,
                    adapter.quota_snapshot(),
                )?;
                self.adoptions_settle(alias);
            }
            None => {
                self.store
                    .set_identity_with_quota(alias, &identity, adapter.quota_snapshot())?;
                // CAD-324: a session the provider did not carry over — a
                // new one, or a reopen whose last turn was lost — starts
                // its next turn with a continuity pack. An adopted
                // endpoint is the same live session: nothing is due.
                let reason = if agent.thread_id.as_deref() != Some(identity.thread_id.as_str()) {
                    Some(crate::continuity::Reason::New)
                } else if self.store.last_turn_lost(alias)? {
                    Some(crate::continuity::Reason::Lost)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    self.continuity_due
                        .lock()
                        .unwrap()
                        .insert(alias.to_string(), reason);
                }
            }
        }
        // CAD-230: a managed provider's process is enrolled for build
        // slots from the pid just recorded — the daemon's own record,
        // never a caller's claim.
        self.enroll_endpoint(alias);
        // A provider's own local tools can only authenticate after this
        // enrollment is visible to the daemon's peer-ancestry verifier.
        // Keep dispatch gated until that proof succeeds.
        adapter.post_enrollment_ready(agent)?;
        self.wake();
        Ok(adapter)
    }

    fn actor_inner(self: &Arc<Self>, alias: &str, ctl: &Arc<AgentCtl>) -> Result<()> {
        let mut agent = self.store.agent(alias)?;
        // CAD-1380: the exact snapshot about to open must be the principal's
        // canonical root before any adapter grant is built.
        if let Some(binding) = self.store.assistant_for_alias(alias)? {
            self.assistant_cwd_matches(&binding, Some(&agent))?;
        }
        // A lost poll on a live Devin Cloud session is not a dead
        // process. Finish the message unknown and keep the actor and
        // endpoint; do not fence.
        let hold_cloud = agent.provider == "devin" && agent.endpoint_kind == "cloud";
        let mut adapter = self.open_actor_adapter(alias, ctl, &agent)?;
        let retry_base = self.pty_retry_base();
        let mut gate_notice: Option<String> = None;
        let mut gate_waits: u32 = 0;
        // Proven paste misses per message — a TUI that looks idle but
        // keeps dropping pastes must not be re-fed forever.
        let mut unrendered: u32 = 0;
        let mut unrendered_message: Option<String> = None;
        let mut cloud_held: Option<Box<store::Message>> = None;
        let mut recover_delay = Duration::from_secs(1);
        let mut recover_started: Option<Instant> = None;
        let mut recover_escalated = false;
        let mut recover_failures: u32 = 0;
        // A hold lives only in this actor. A daemon restart or a stop
        // during a hold leaves the message `unknown`, and start, resume
        // and relaunch fence the agent on it: the operator inspects the
        // Devin session and reconciles. Nothing rebuilds the turn.
        loop {
            if self.closing.load(Ordering::SeqCst) {
                return Ok(());
            }
            // CAD-250: a delivered turn whose report never came is
            // bounded here, on the actor that owns the endpoint — it goes
            // `unknown` and fences exactly like any other uncertain
            // outcome. Checked every pass: the queue wait below is
            // bounded (5s idle, 30s gate backoff), so the bound fires
            // within seconds of running out.
            if let Some(awaiting) = self.report_overdue(alias) {
                if self.report_timeout(alias, awaiting)? {
                    return Err(Error::unknown(UNKNOWN_GENERIC_REASON));
                }
            }
            if let Some(pending) = cloud_held.clone() {
                if !self.store.agent(alias)?.enabled {
                    return Ok(());
                }
                // An operator reconcile settled the held message: stop
                // polling and take the next queued message.
                if self
                    .store
                    .message(&pending.id)?
                    .is_none_or(|message| message.state != "unknown")
                {
                    cloud_held = None;
                    ctl.cloud_held.store(false, Ordering::SeqCst);
                    recover_started = None;
                    recover_escalated = false;
                    recover_failures = 0;
                    continue;
                }
                // After the budget, one escalation and no further polls.
                // The agent stays enabled and the held message is not replayed.
                if recover_escalated {
                    ctl.wake.wait_if_unchanged(
                        ctl.wake.ticket(),
                        Instant::now() + Duration::from_secs(3600),
                    );
                    continue;
                }
                let transient = match adapter.poll_settled() {
                    // A stop or shutdown released the adapter, which then
                    // reports interrupted. That is not the session's
                    // outcome: leave the message for the reconcile.
                    Ok(SettledPoll::Ready(_))
                        if self.closing.load(Ordering::SeqCst)
                            || !self.store.agent(alias)?.enabled =>
                    {
                        return Ok(());
                    }
                    Ok(SettledPoll::Ready(turn)) => {
                        let status = match turn.status.as_str() {
                            "failed" => "failed",
                            "interrupted" => "interrupted",
                            _ => "completed",
                        };
                        let note = if turn.text.is_empty() {
                            None
                        } else {
                            Some(turn.text.as_str())
                        };
                        if let Err(error) =
                            self.store
                                .reconcile(&pending.id, status, note, "cloud_poll", None)
                        {
                            // The operator reconciled it first; theirs stands.
                            let settled = self
                                .store
                                .message(&pending.id)?
                                .is_none_or(|message| message.state != "unknown");
                            if !settled {
                                return Err(error);
                            }
                        }
                        cloud_held = None;
                        ctl.cloud_held.store(false, Ordering::SeqCst);
                        recover_started = None;
                        recover_escalated = false;
                        recover_failures = 0;
                        self.wake();
                        continue;
                    }
                    Ok(SettledPoll::Pending { transient }) => transient,
                    Err(_) => true,
                };
                let interval = adapter.poll_interval().max(Duration::from_millis(1));
                if transient {
                    recover_failures = recover_failures.saturating_add(1);
                    recover_delay = recover_delay
                        .saturating_mul(2)
                        .clamp(interval, Duration::from_secs(60));
                } else {
                    recover_delay = interval;
                }
                let started = recover_started.get_or_insert_with(Instant::now);
                if recover_failures >= 8 || started.elapsed() >= adapter.recover_budget() {
                    self.store.escalate_cloud_hold(
                        &pending,
                        "devin cloud recovery stopped after repeated poll failures",
                    )?;
                    recover_escalated = true;
                    self.wake();
                    continue;
                }
                let ready_at = Instant::now() + recover_delay;
                while Instant::now() < ready_at {
                    if self.closing.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    ctl.wake.wait_if_unchanged(ctl.wake.ticket(), ready_at);
                }
                continue;
            }
            // Sample before the empty check. `disconnected` probes the
            // pane before the wait; a `notify_agent` in that gap must
            // still wake the actor or a routed reply sits until the
            // 5s poll.
            let ticket = ctl.wake.ticket();
            // CAD-561: while an update drains, no actor starts a new
            // turn — queued deliveries stay in the inbox and are claimed
            // after the restart (or when the drain is lifted). The
            // in-flight turn this actor already runs is untouched.
            if self.draining() {
                if !self.store.agent(alias)?.enabled {
                    return Ok(());
                }
                ctl.wake
                    .wait_if_unchanged(ticket, Instant::now() + self.idle_poll);
                continue;
            }
            match self.take_app_aware(alias)? {
                Take::Stop => return Ok(()),
                Take::Empty => {
                    if adapter.disconnected() {
                        // Submitted PTY messages may have reached the
                        // provider; fence them rather than replay.
                        let _ = self
                            .store
                            .orphan_running(alias, "endpoint lost after submission");
                        return Err(Error::unknown("Provider process disconnected while idle"));
                    }
                    ctl.wake
                        .wait_if_unchanged(ticket, Instant::now() + self.idle_poll);
                }
                Take::Message(message) => {
                    // A gate refusal requeues the same message, so its
                    // proven render-miss budget must survive the wait. If a
                    // queued row was cancelled while waiting, start the next
                    // row with a fresh budget instead of inheriting its count.
                    if unrendered_message.as_deref() != Some(message.id.as_str()) {
                        unrendered = 0;
                        unrendered_message = Some(message.id.clone());
                    }
                    // CAD-1752: a confined catalogue agent runs each ticket
                    // in that ticket's lane worktree, by relaunching between
                    // turns - never mid-turn, never unconfined. The message
                    // goes back to the queue while the endpoint is swapped.
                    if !message.is_nudge() {
                        match self.lane_workdir_needed(&agent, &message) {
                            Ok(None) => {}
                            Ok(Some(workdir)) => {
                                let _ = self.store.requeue(&message.id);
                                let _ = self.store.set_agent_state_if(alias, "idle", "busy");
                                if let Err(error) = self.relaunch_for_lane(
                                    ctl,
                                    &mut agent,
                                    &mut adapter,
                                    &workdir,
                                    &message,
                                ) {
                                    // The endpoint could not be reopened in the
                                    // lane: settle this message once, with the
                                    // reason, and fence the agent on the error.
                                    // No second attempt, no fresh session, no
                                    // wider grant.
                                    let reason = format!(
                                        "relaunch for {} failed: {error}",
                                        message.issue.as_deref().unwrap_or("the task")
                                    );
                                    let failed = json!({"status": "failed", "text": "",
                                                "error": reason});
                                    self.store.finish(
                                        &message,
                                        "failed",
                                        &failed,
                                        Some(&reason),
                                    )?;
                                    self.notify_routed_target(&message, &failed);
                                    self.wake();
                                    // The fence carries the stated reason, not
                                    // the closed-connection error underneath.
                                    return Err(Error::provider(reason));
                                }
                                continue;
                            }
                            Err(Error::PreWrite(reason)) => {
                                let failed = json!({"status": "failed", "text": "",
                                            "error": reason});
                                self.store
                                    .finish(&message, "failed", &failed, Some(&reason))?;
                                self.notify_routed_target(&message, &failed);
                                self.wake();
                                continue;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    let started_id = message.id.clone();
                    let shared = Arc::clone(self);
                    let watch = Arc::clone(ctl);
                    // Routed mail and nudges may pass the pty claim gate
                    // while a turn runs. Clear on return, including errors,
                    // so a later user message cannot inherit the flag.
                    let nudge = message.is_nudge();
                    // CAD-324: a nudge owns no turn and carries no pack.
                    // CAD-1009: a scoped App turn's token slot (None for
                    // a nudge, a plain message or an endpoint that
                    // cannot redeem).
                    // CAD-1098 I7: a session serves one conversation — switch
                    // (new provider session, profile and pack) before the
                    // prompt is built, so the pack is this conversation's.
                    if !nudge {
                        self.switch_session_if_needed(alias, &adapter, &message)?;
                    }
                    let slot = if nudge {
                        None
                    } else {
                        self.turn_slot(&agent, &message)
                    };
                    let mut pending_context: Option<ContextDelivery> = None;
                    let prompt = if nudge {
                        message.body.clone()
                    } else {
                        let compacted = self.context_compacted(&message);
                        let (base, pack) = self.continuity_parts(
                            alias,
                            &agent.endpoint_kind,
                            &message,
                            slot.as_deref(),
                        );
                        // The committed actor profile follows the request on
                        // every real turn, including after provider `/new`.
                        // Missing or unreadable profiles fail closed.
                        let body = self.prompt_with_profile(&message, base)?;
                        // CAD-1822: mandatory task context is planned against
                        // the delivery body and actor profile first; optional
                        // continuity history is budgeted from what remains.
                        let context = match self
                            .context_delivery(&agent, &message, &body, false, compacted)
                        {
                            Ok(context) => context,
                            Err(Error::PreWrite(reason)) => {
                                let failed = json!({"status": "failed", "text": "",
                                                    "error": reason});
                                self.store
                                    .finish(&message, "failed", &failed, Some(&reason))?;
                                self.notify_routed_target(&message, &failed);
                                gate_notice = None;
                                self.wake();
                                continue;
                            }
                            Err(error) => return Err(error),
                        };
                        let mut text = match context {
                            Some(delivery) => {
                                let text = if delivery.plan.text.is_empty() {
                                    body
                                } else {
                                    format!("{body}\n\n{}", delivery.plan.text)
                                };
                                pending_context = Some(delivery);
                                text
                            }
                            None => body,
                        };
                        // CAD-1822: the optional continuity prefix yields
                        // first. Include it whole only when it still fits;
                        // otherwise omit it whole with a recorded reason and
                        // keep the mandatory task contract and sources.
                        if let Some(pack) = pack {
                            let reserved = pending_context
                                .as_ref()
                                .map(|delivery| delivery.reserved_bytes)
                                .unwrap_or(0);
                            let combined = text.len() + pack.text.len() + 2 + reserved;
                            if pending_context.is_some()
                                && combined > crate::context_pack::MAX_COMBINED_BYTES
                            {
                                self.omit_continuity(alias, &message, &pack, combined);
                            } else {
                                self.record_continuity(alias, &message, &pack);
                                text = pack.wrap(&text);
                            }
                        }
                        text
                    };
                    adapter.set_unclaimed_ok(message.is_routed() || nudge);
                    // CAD-520: a nudge may also enter through a busy
                    // pane's steering input (Devin's guide box). Cleared
                    // with `unclaimed_ok` so a later message cannot
                    // inherit either flag.
                    adapter.set_steer_ok(nudge);
                    // CAD-565: the pane may receive only a bounded
                    // notice — the stored body still faces the
                    // endpoint's own screen (a pty profile's
                    // literal-only checks) before it may deliver.
                    let outcome = self
                        .admit_app_submission(&message)
                        .and_then(|()| adapter.check_body(&message.body))
                        .and_then(|()| {
                            if !nudge {
                                let images =
                                    self.turn_images_for(&message).map_err(
                                        |error| match error {
                                            Error::Rejected(reason) => Error::PreWrite(reason),
                                            error => error,
                                        },
                                    )?;
                                adapter.bind_turn_images(&images)?;
                            }
                            let on_started = move |turn: &str| {
                                // CAD-250: a nudge owns no turn — it never
                                // becomes `running`, and its paste is not the
                                // held turn's proof of life.
                                if !nudge {
                                    let _ = shared.store.mark_running(&started_id, turn);
                                    watch.bump_activity();
                                }
                                shared.wake();
                            };
                            let first = adapter.run_turn_slotted(
                                &prompt,
                                slot.as_deref(),
                                &message.id,
                                &on_started,
                            )?;
                            if nudge || !adapter.reset_rejected_session(&first)? {
                                return Ok(first);
                            }
                            // CAD-1076: one retry on a fresh session.
                            // Offered only for a master history refusal
                            // before any tool. A serialized-body refusal
                            // after a tool is not offered — retrying
                            // would replay work that already ran, and
                            // that turn stays failed with its limitation.
                            let (base, pack) = self.after_session_reset_parts(
                                &agent,
                                &message,
                                slot.as_deref(),
                                &first,
                            );
                            // Same priority as the first attempt: the actor
                            // profile and mandatory task context are planned
                            // against the base body; optional history yields.
                            let base = self.prompt_with_profile(&message, base)?;
                            pending_context = None;
                            let prompt =
                                match self.context_delivery(&agent, &message, &base, true, false) {
                                    Ok(Some(delivery)) => {
                                        let text = if delivery.plan.text.is_empty() {
                                            base
                                        } else {
                                            format!("{base}\n\n{}", delivery.plan.text)
                                        };
                                        pending_context = Some(delivery);
                                        text
                                    }
                                    Ok(None) => base,
                                    Err(error) => return Err(error),
                                };
                            let prompt = match pack {
                                Some(pack) => {
                                    let reserved = pending_context
                                        .as_ref()
                                        .map(|delivery| delivery.reserved_bytes)
                                        .unwrap_or(0);
                                    let combined = prompt.len() + pack.text.len() + 2 + reserved;
                                    if pending_context.is_some()
                                        && combined > crate::context_pack::MAX_COMBINED_BYTES
                                    {
                                        self.omit_continuity(alias, &message, &pack, combined);
                                        prompt
                                    } else {
                                        self.record_continuity(alias, &message, &pack);
                                        pack.wrap(&prompt)
                                    }
                                }
                                None => prompt,
                            };
                            adapter.run_turn_slotted(
                                &prompt,
                                slot.as_deref(),
                                &message.id,
                                &on_started,
                            )
                        });
                    adapter.set_unclaimed_ok(false);
                    adapter.set_steer_ok(false);
                    // CAD-250: an unconfirmed nudge paste ends `unknown`,
                    // but a nudge belongs to no turn — it never fences the
                    // agent, never retries, never touches the held turn.
                    let outcome = match outcome {
                        Err(Error::NotRendered(miss)) if nudge => {
                            let _ = self.store.event_public(
                                alias,
                                "paste_not_rendered",
                                json!({"message": message.id,
                                       "reason": miss.reason,
                                       "attempt": 1,
                                       "retry": false,
                                       "before": miss.before_tail,
                                       "after": miss.after_tail,
                                       "claim_probe": miss.claim_probe,
                                       "reprobe": miss.reprobe}),
                            );
                            self.nudge_unconfirmed(&message, &miss.reason)?;
                            continue;
                        }
                        Err(Error::OutcomeUnknown(reason)) if nudge => {
                            self.nudge_unconfirmed(&message, &reason)?;
                            continue;
                        }
                        other => other,
                    };
                    match outcome {
                        Ok(result) => {
                            if let Err(error) = self.complete(&message, result) {
                                // The provider reported an outcome we could
                                // not persist or classify — ambiguous
                                // post-submission, fence rather than replay.
                                return self.unknown(
                                    alias,
                                    &message,
                                    &format!("reported outcome could not be applied: {error}"),
                                );
                            }
                            // CAD-1822: only a definite provider result
                            // confirms supply. The receipt names the actual
                            // session/generation and newly appended bytes.
                            if let Some(delivery) = pending_context.take() {
                                if let Err(error) = delivery.record(&self.store, &message) {
                                    let _ = self.store.event_public(
                                        alias,
                                        "context_supply_failed",
                                        json!({"message": message.id,
                                               "error": error.to_string()}),
                                    );
                                }
                            }
                            gate_notice = None;
                            gate_waits = 0;
                            unrendered = 0;
                            unrendered_message = None;
                        }
                        // The paste did not render within the deadline:
                        // evidence of a dropped or unsubmitted delivery.
                        // Routed notifications are informational — requeue
                        // for at-least-once delivery, bounded; on exhaustion
                        // the delivery is *parked*, not fenced: a
                        // notification must never kill the recipient's pane
                        // and in-flight work. The worker's result stays
                        // durable on the worker's own message, so nothing
                        // is lost. Task messages keep the uncertainty
                        // discipline: `unknown` + fence, never a blind
                        // replay.
                        Err(Error::NotRendered(miss)) => {
                            unrendered += 1;
                            let routed = message.is_routed();
                            let retry = routed && unrendered <= 3;
                            // The miss carries the pane's own evidence:
                            // screen tails before the paste and after
                            // the deadline plus the probe verdict that
                            // admitted the send — what "idle" looked
                            // like when delivery failed.
                            let crate::error::RenderMiss {
                                reason,
                                before_tail,
                                after_tail,
                                claim_probe,
                                reprobe,
                            } = *miss;
                            let _ = self.store.event_public(
                                alias,
                                "paste_not_rendered",
                                if message.source == "app_run_dispatch" {
                                    json!({"message":message.id,"app_owned":true,"reason":"app paste not rendered","attempt":unrendered,"retry":retry})
                                } else { json!({"message": message.id,
                                       "reason": reason,
                                       "attempt": unrendered,
                                       "retry": retry,
                                       "before": before_tail,
                                       "after": after_tail,
                                       "claim_probe": claim_probe,
                                       "reprobe": reprobe}) },
                            );
                            if retry {
                                let retry_ticket = ctl.wake.ticket();
                                let _ = self.store.requeue(&message.id);
                                let _ = self.store.set_agent_state_if(alias, "idle", "busy");
                                gate_notice = None;
                                ctl.wake
                                    .wait_if_unchanged(retry_ticket, Instant::now() + retry_base);
                            } else if routed {
                                let _ = self.store.event_public(
                                    alias,
                                    "delivery_parked",
                                    json!({"message": message.id,
                                           "reason": reason,
                                           "attempts": unrendered}),
                                );
                                let parked = json!({"status": "failed",
                                            "via": "pty_render_miss",
                                            "error": reason});
                                self.store
                                    .finish(&message, "failed", &parked, Some(&reason))?;
                                self.notify_routed_target(&message, &parked);
                                let _ = self.store.set_agent_state_if(alias, "idle", "busy");
                                unrendered = 0;
                                unrendered_message = None;
                                self.wake();
                            } else {
                                return self.unknown(
                                    alias,
                                    &message,
                                    "submission accepted but never rendered on the endpoint",
                                );
                            }
                        }
                        // The submission gate refused before any paste:
                        // safe to retry — back to the queue with a
                        // bounded backoff, never a silent drop.
                        Err(Error::GateRefused(reason)) => {
                            let retry_ticket = ctl.wake.ticket();
                            let _ = self.store.requeue(&message.id);
                            let _ = self.store.set_agent_state_if(alias, "idle", "busy");
                            if gate_notice.as_deref() != Some(reason.as_str()) {
                                let _ = self.store.event_public(
                                    alias,
                                    "gate_wait",
                                    json!({"message": message.id, "reason": reason}),
                                );
                                gate_notice = Some(reason);
                            }
                            // 5s → 10 → 20 → 30s cap (`gate_backoff`):
                            // claims and inbox arrivals wake the wait
                            // early, so the poll is only the fallback for
                            // a busy pane.
                            let wait = gate_backoff(retry_base, gate_waits);
                            gate_waits = gate_waits.saturating_add(1);
                            ctl.wake
                                .wait_if_unchanged(retry_ticket, Instant::now() + wait);
                        }
                        Err(Error::OutcomeUnknown(error)) => {
                            if hold_cloud {
                                let held = json!({
                                    "status": "unknown",
                                    "text": "",
                                    "error": error,
                                    "held": true,
                                });
                                ctl.cloud_held.store(true, Ordering::SeqCst);
                                self.store
                                    .finish(&message, "unknown", &held, Some(&error))?;
                                let _ = self.store.event_public(
                                    alias,
                                    "cloud_hold",
                                    json!({"reason": error, "fenced": false}),
                                );
                                cloud_held = Some(message);
                                recover_delay =
                                    adapter.poll_interval().max(Duration::from_millis(1));
                                recover_started = Some(Instant::now());
                                recover_escalated = false;
                                recover_failures = 0;
                                self.wake();
                            } else {
                                return self.unknown(alias, &message, &error);
                            }
                        }
                        // Deterministic pre-submission rejection: zero
                        // bytes reached the provider, so nothing is
                        // unknown — fail the message, keep the endpoint
                        // live and keep draining the queue.
                        Err(Error::PreWrite(reason)) => {
                            let failed = json!({"status": "failed", "text": "",
                                        "error": reason});
                            self.store
                                .finish(&message, "failed", &failed, Some(&reason))?;
                            self.notify_routed_target(&message, &failed);
                            gate_notice = None;
                            self.wake();
                        }
                        // A provider/adapter error is actor-fatal: record
                        // the failed attempt, then land in `attention`.
                        Err(error) => {
                            let failed =
                                json!({"status": "failed", "text": "", "error": error.to_string()});
                            self.store.finish(
                                &message,
                                "failed",
                                &failed,
                                Some(&error.to_string()),
                            )?;
                            self.notify_routed_target(&message, &failed);
                            // Other submitted PTY messages are now
                            // orphaned by the dead endpoint.
                            let _ = self
                                .store
                                .orphan_running(alias, "endpoint lost after submission");
                            self.wake();
                            // CAD-1142: stamp the claimed turn's proven
                            // source onto the fatal error so `run_actor`
                            // can classify its public fence text without
                            // reading provider prose. The stamped text is
                            // unchanged — only the public writes differ.
                            let error = if message.source == "app_run_dispatch" {
                                error.into_app_owned_fatal()
                            } else {
                                error
                            };
                            return Err(error);
                        }
                    }
                }
            }
        }
    }

    /// CAD-250: a nudge whose paste could not be confirmed — `unknown`,
    /// recorded with a `nudge_unconfirmed` event, and nothing else: no
    /// fence, no retry, no notice (a nudge has no `reply_to`).
    fn nudge_unconfirmed(&self, message: &Message, reason: &str) -> Result<()> {
        let stored = json!({"status": "unknown", "text": "", "error": reason,
                            "via": "pty_nudge"});
        self.store
            .finish(message, "unknown", &stored, Some(reason))?;
        let _ = self.store.event_public(
            &message.alias,
            "nudge_unconfirmed",
            json!({"message": message.id, "reason": reason}),
        );
        // `finish` idles the agent; a turn still held keeps it busy.
        if !self.store.held_turns(&message.alias)?.is_empty() {
            let _ = self
                .store
                .set_agent_state_if(&message.alias, "busy", "idle");
        }
        self.wake();
        Ok(())
    }

    /// CAD-250: a nudge completes at its confirmed paste — no report
    /// is owed, and the held turn (if any) is untouched.
    pub(super) fn complete(&self, message: &Message, result: TurnResult) -> Result<()> {
        if message.is_nudge() {
            let delivered = json!({"status": "completed", "via": "pty_nudge",
                        "turn_id": result.turn_id});
            self.store.finish(message, "completed", &delivered, None)?;
            if !self.store.held_turns(&message.alias)?.is_empty() {
                let _ = self
                    .store
                    .set_agent_state_if(&message.alias, "busy", "idle");
            }
            self.wake();
            return Ok(());
        }
        // PTY endpoints report "submitted": the paste reached the
        // terminal, but only an explicit ack/result report may finish
        // the message — it stays `running` meanwhile.
        if result.status == "submitted" {
            self.store.mark_submitted(message)?;
            // A routed notification's delivery IS its completion — the
            // receiving PM is not expected to report a result on it.
            if message.is_routed() {
                let delivered = json!({"status": "completed", "via": "pty_deliver",
                            "turn_id": result.turn_id});
                self.store.finish(message, "completed", &delivered, None)?;
                self.notify_routed_target(message, &delivered);
            }
            self.wake();
            return Ok(());
        }
        let status = match result.status.as_str() {
            "completed" | "failed" | "interrupted" => result.status.clone(),
            other => {
                return Err(Error::unknown(format!(
                    "Unexpected provider completion status: {other}"
                )))
            }
        };
        let stored = json!({
            "turn_id": result.turn_id,
            "status": status,
            "text": result.text,
            "stop_reason": result.stop_reason,
            "error": result.error,
        });
        if message.source == "app_run_dispatch" && status == "completed" {
            let (run, _) = self
                .store
                .app_message_installation(&message.id)?
                .ok_or_else(|| Error::rejected("app completion association is absent"))?;
            let recorded = self.with_app_run_current(&run, |digest| {
                self.store.app_message_admit(message, digest)?;
                self.store
                    .finish(message, &status, &stored, result.error.as_deref())
            });
            if recorded.is_err() {
                // Preserve transport evidence without accepting stale material.
                self.store.finish(message, "failed", &json!({"status":"failed","turn_id":stored["turn_id"],"reason":"app authority changed before material acceptance"}), Some("app authority changed before material acceptance"))?;
            }
        } else {
            self.store
                .finish(message, &status, &stored, result.error.as_deref())?;
        }
        self.notify_routed_target(message, &stored);
        self.wake();
        Ok(())
    }

    /// CAD-250: the agent's delivered-unreported turns and their bound
    /// when at least one has run out — `None` while every one is within
    /// it (or the bound is `0`, disabled).
    fn report_overdue(&self, alias: &str) -> Option<(Vec<Message>, u64)> {
        // Every turn-holding row, marked `awaiting_report` or not (F2): the
        // hold and the bound share one predicate.
        let awaiting = self.store.held_turns(alias).ok()?;
        if awaiting.is_empty() {
            return None;
        }
        let agent = self.store.agent(alias).ok()?;
        if agent.endpoint_kind != "pty" {
            return None;
        }
        let bound = store::report_timeout_secs(agent.params.as_ref());
        let now = epoch_secs();
        awaiting
            .iter()
            .any(|m| m.report_overdue(bound, now))
            .then_some((awaiting, bound))
    }

    /// CAD-250: retire overdue unreported turns to `unknown` and fence
    /// the actor — the standard uncertain-outcome path, never a
    /// completion and never a replay. Each overdue row gets one
    /// `report_timeout` event and, through the `unknown` finish, exactly
    /// one `worker_notice` to its `reply_to` (a job kickoff's is the PM;
    /// `send` defaults it to the upstream). Siblings still within the
    /// bound — only rows that accumulated before one-turn-per-actor —
    /// go `unknown` with it, since the endpoint they ran on is being
    /// detached. Every write is guarded: a report that lands first wins.
    /// `Ok(true)` when anything went `unknown` (the caller exits fenced).
    fn report_timeout(&self, alias: &str, (awaiting, bound): (Vec<Message>, u64)) -> Result<bool> {
        let now = epoch_secs();
        let (overdue, siblings): (Vec<&Message>, Vec<&Message>) =
            awaiting.iter().partition(|m| m.report_overdue(bound, now));
        let mut expired: Vec<String> = Vec::new();
        for m in overdue {
            let waited = m.report_clock().map_or(0, |c| (now - c).max(0.0) as u64);
            let reason = format!(
                "no report within report_timeout_secs={bound} ({}) of delivery \
                 — outcome unknown; not completed, not replayed",
                fmt_duration(bound)
            );
            if self
                .store
                .expire_awaiting_report(&m.id, Some((bound, now)), &reason)?
            {
                let (job_id, task_id) = self.message_scope(m);
                let _ = self.store.event_public_scoped(
                    alias,
                    "report_timeout",
                    json!({"message": m.id, "turn_id": m.turn_id,
                           "waited_secs": waited, "report_timeout_secs": bound}),
                    job_id.as_deref(),
                    task_id,
                );
                self.notify_routed_target(m, &Value::Null);
                expired.push(m.id.clone());
            }
        }
        let Some(first) = expired.first().cloned() else {
            return Ok(false);
        };
        for m in siblings {
            let reason = format!(
                "fenced with {first}, whose report bound ran out — outcome \
                 unknown; not completed, not replayed"
            );
            if self.store.expire_awaiting_report(&m.id, None, &reason)? {
                self.notify_routed_target(m, &Value::Null);
                expired.push(m.id.clone());
            }
        }
        let reason = format!(
            "no report within report_timeout_secs={bound} ({}) — {} turn(s) went unknown: {}",
            fmt_duration(bound),
            expired.len(),
            expired.join(", ")
        );
        // One write, as in `unknown`: the fence lands with the cleared
        // endpoint.
        self.store
            .set_state_detached(alias, "attention", Some(&format_unknown_fence(&reason)))?;
        let _ = self
            .store
            .event_public(alias, "attention", json!({"reason": reason}));
        self.wake();
        Ok(true)
    }

    /// An `OutcomeUnknown` never becomes a retry: mark the attempt and
    /// fence the actor for review. `reason` is the provider's own account
    /// of the uncertainty (idle window, EOF, cap exceeded, …) — the
    /// operator needs it to reconcile.
    ///
    /// CAD-1142 reason privacy: for an app-owned turn (`app_run_dispatch`)
    /// the provider's account may carry credentials or prose, so it is
    /// never published on surfaces any `agent_events`/`agent_show` reader
    /// can see — the public `attention` event and the agent row's `error`
    /// carry the bounded Cadence-authored class instead. The raw account
    /// stays on the message row, whose `message read` for an app source
    /// already requires operator proof (`rpc_message_read`), so the
    /// operator still reconciles from the full detail.
    pub(super) fn unknown(&self, alias: &str, message: &Message, reason: &str) -> Result<()> {
        // CAD-1353: the provider's idle fence is the real unknown-outcome
        // diagnostic. Name the budget this dispatch was given and the
        // last provider event we can point at, not only the idle window.
        let budget = self.store.message_stall_budget(message).or_else(|| {
            self.store.agent(alias).ok().and_then(|agent| {
                agent
                    .params
                    .as_ref()
                    .and_then(|params| params.get("stall_secs"))
                    .and_then(|value| {
                        value
                            .as_i64()
                            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
                    })
            })
        });
        let budget = budget
            .map(|secs| format!("{secs}s"))
            .unwrap_or_else(|| "default".to_string());
        let last_event = self
            .provider_activity_epoch(alias)
            .or_else(|| self.last_activity_epoch(alias))
            .or_else(|| {
                self.store
                    .message(&message.id)
                    .ok()
                    .flatten()
                    .and_then(|current| current.started)
            })
            .or(message.started)
            .map(|at| format!("last observed activity at {at:.0}"))
            .unwrap_or_else(|| "none recorded".to_string());
        // Lead with the Cadence-authored evidence: the fence formatter
        // bounds the detail, and a long provider account must not push
        // the required budget/activity out of the bounded text.
        let fence_reason =
            format!("dispatch stall budget {budget}; last provider event: {last_event}; {reason}");
        // The operator-private row keeps the provider's own account
        // verbatim; the budget/activity evidence is fence text.
        let stored = json!({"status": "unknown", "text": "", "error": reason});
        self.store
            .finish(message, "unknown", &stored, Some(reason))?;
        self.notify_routed_target(message, &stored);
        let public_reason = if message.source == "app_run_dispatch" {
            store::app_runs::app_uncertain_turn_reason()
        } else {
            fence_reason
        };
        // One write: the fence is visible immediately, so the cleared
        // endpoint must land with it — a reader in between must never
        // see `attention` plus a live endpoint.
        self.store.set_state_detached(
            alias,
            "attention",
            Some(&format_unknown_fence(&public_reason)),
        )?;
        let _ = self
            .store
            .event_public(alias, "attention", json!({"reason": public_reason}));
        self.wake();
        Err(Error::unknown(UNKNOWN_GENERIC_REASON))
    }
}
