//! CAD-534: `cadence daemon` messages RPC handlers — moved verbatim from src/daemon.rs; the
//! item→file map is src/daemon/split-map.toml
//! (scripts/split-daemon regenerates it).

use super::*;

use crate::adapter::InterruptOutcome;
use crate::adapter::Probe;
use crate::peer::PeerTies;

impl Shared {
    /// `agent_send` without a connection to attribute (unit tests):
    /// a threaded agent records the message as unattributed, and a
    /// steering send is the operator's.
    #[cfg(test)]
    pub(super) fn rpc_send(self: &Arc<Self>, params: &Value) -> Result<Value> {
        self.send_with(params, &|_| Ok(store::Sender::Unattributed), &|_| {
            Ok(AgentCaller::Operator)
        })
    }

    /// `agent_send` over the socket: a threaded agent's chat records
    /// who queued it, derived from the connection (CAD-319); a steering
    /// send's caller is derived the same way (CAD-158).
    pub(super) fn rpc_send_from(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        self.send_with(
            params,
            &|alias| self.thread_sender(alias, peer_pid),
            &|verb| self.agent_caller(peer_pid, verb),
        )
    }

    /// Who queued a message for `alias`'s thread. Only computed for an
    /// alias that has one — the identity walk is not free. An agent
    /// connection is that agent; an underivable caller is unattributed.
    /// A connection tied to no agent would write into the thread as the
    /// operator, so it must be provably the operator (CAD-384, CAD-276):
    /// a detached child of an agent is refused, before anything is
    /// written.
    pub(super) fn thread_sender(&self, alias: &str, peer_pid: u32) -> Result<store::Sender> {
        match self.store.thread(alias) {
            Ok(Some(_)) => {}
            _ => return Ok(store::Sender::Unattributed),
        }
        Ok(match self.caller_identity(peer_pid) {
            Ok(Caller::Agent(v)) => store::Sender::Agent(v.agent.alias.clone()),
            Ok(Caller::NoAgentIdentity) => {
                self.proven_operator("send into an operator thread", peer_pid)?;
                store::Sender::Operator
            }
            Err(_) => store::Sender::Unattributed,
        })
    }

    /// A send the daemon itself builds (review routing, master relays,
    /// thread send): it never steers, so no steering caller exists.
    pub(super) fn send_as(
        self: &Arc<Self>,
        params: &Value,
        sender_of: &dyn Fn(&str) -> Result<store::Sender>,
    ) -> Result<Value> {
        self.send_with(params, sender_of, &|verb| {
            Err(Error::rejected(format!(
                "{verb} refused: this send carries no caller to steer with"
            )))
        })
    }

    /// `send_as` with the caller a steering send (CAD-158) is authorized
    /// against, derived from the connection by `steer_caller`.
    pub(super) fn send_with(
        self: &Arc<Self>,
        params: &Value,
        sender_of: &dyn Fn(&str) -> Result<store::Sender>,
        steer_caller: &dyn Fn(&str) -> Result<AgentCaller>,
    ) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let text = required_str(params, "text")?;
        let target = self.store.agent_opt(&alias)?;
        // CAD-158: `--priority urgent` / `--supersedes` steer the
        // recipient's queue — the operator or its own PM only
        // (`AgentMutation::Steer`), the caller derived from the
        // connection like every agent mutation, never from a field.
        let priority = optional_text(params, "priority")?
            .map(store::Priority::parse)
            .transpose()?
            .unwrap_or_default();
        let supersedes: Vec<String> = match params.get("supersedes") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(ids)) => ids
                .iter()
                .map(|id| {
                    id.as_str().map(str::to_string).ok_or_else(|| {
                        Error::rejected("supersedes must be an array of message ids")
                    })
                })
                .collect::<Result<_>>()?,
            Some(_) => {
                return Err(Error::rejected(
                    "supersedes must be an array of message ids",
                ))
            }
        };
        // CAD-250: `--nudge` — turnless steering for a live pty pane. The
        // flag is the only way in: a caller-supplied `source: "nudge"`
        // takes the same checks rather than bypassing them.
        let nudge = params
            .get("nudge")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || optional_str(params, "source") == Some(store::NUDGE_SOURCE);
        let mut steer = store::Steer {
            priority,
            supersedes: &supersedes,
            ..store::Steer::NONE
        };
        // CAD-520 r3: a nudge steers a live pane mid-turn — the same
        // queue mutation as --priority/--supersedes — so it takes the
        // same caller rule, derived from the connection, never a field.
        let steering_caller = if steer.is_steering() || nudge {
            let verb = if steer.is_steering() {
                "send --priority/--supersedes"
            } else {
                "send --nudge"
            };
            reject_identity_fields(params, verb)?;
            let target = target
                .as_ref()
                .ok_or_else(|| Error::rejected("Unknown managed agent"))?;
            let caller = steer_caller(verb)?;
            let pm = self.effective_pm(target)?;
            crate::peer::may_mutate_agent(
                &caller,
                &alias,
                pm.as_deref(),
                AgentMutation::Steer,
                verb,
            )
            .map_err(Error::rejected)?;
            Some(caller)
        } else {
            None
        };
        if let Some(caller) = &steering_caller {
            (steer.by, steer.by_kind) = caller.audit();
        }
        // CAD-467 wrote `issue`/`worktree` on the message row at send,
        // steer-gated — but the steer gate checks caller-versus-target,
        // never the values, so a PM could mark a send to its own worker
        // with a forged lane (CAD-378 R6). Now only dispatch paths write
        // the tags: `dispatch_send` sets them from the lane the daemon
        // itself resolves, `task_dispatch` from the job's rows. A caller
        // field is refused outright — silently dropping it would let a
        // retry pass as untagged and mis-suppress the reported-kickoff
        // duplicate check, and would hide the forgery attempt.
        if optional_str(params, "issue").is_some() || optional_str(params, "worktree").is_some() {
            let verb = "send --issue/--worktree";
            reject_identity_fields(params, verb)?;
            return Err(Error::rejected(format!(
                "{verb} refused: only a dispatch sets a message's lane tags — \
                 `cadence dispatch` records the issue and lane worktree from \
                 the daemon's own resolution; plain sends carry none"
            )));
        }
        // A pty endpoint pastes literally and fails a body with control
        // characters at delivery; refuse it here so `send` never answers
        // `queued` for a message that cannot be delivered (CAD-218).
        let pty = target.as_ref().is_some_and(|a| a.endpoint_kind == "pty");
        if nudge {
            if steer.is_steering() {
                return Err(Error::rejected(
                    "--nudge is pasted at once and never queued — it takes no \
                     --priority or --supersedes",
                ));
            }
            if optional_str(params, "task").is_some() {
                return Err(Error::rejected(
                    "--nudge is steering, not task work — it takes no --task",
                ));
            }
            if text.chars().count() > NUDGE_MAX_CHARS {
                return Err(Error::rejected(format!(
                    "a nudge is at most {NUDGE_MAX_CHARS} characters — send longer \
                     guidance as a normal message or a file path"
                )));
            }
            if let Some(agent) = target.as_ref().filter(|a| a.endpoint_kind != "pty") {
                return Err(Error::rejected(format!(
                    "--nudge only applies to pty endpoints — '{alias}' is {}/{}; \
                     send it a normal message instead",
                    agent.provider, agent.endpoint_kind
                )));
            }
            if optional_str(params, "reply_to").is_some() {
                return Err(Error::rejected(
                    "--nudge owes no report, so it takes no reply_to",
                ));
            }
            // N1: a nudge is for a pane that exists now — never queued for
            // a stopped or fenced agent to receive later.
            let live = self.lifecycle.lock().unwrap().agents.contains_key(&alias)
                && target.as_ref().is_some_and(|a| {
                    a.endpoint.is_some() && matches!(a.state.as_str(), "idle" | "busy")
                });
            if !live {
                return Err(Error::rejected(format!("agent {alias} has no live pane")));
            }
        }
        if pty && crate::adapter::pty::has_control_chars(text) {
            return Err(Error::rejected(
                "PTY messages must be a single line without control characters \
                 — put a long body in a file and send its path",
            ));
        }
        // `send --task` attaches the delivery to a task — ad-hoc
        // PM↔worker follow-up inside a job's delivery record. CAD-160:
        // an open task's message is composed to restate its objective
        // and outstanding criteria, fitted to the endpoint's ceiling.
        let task = optional_str(params, "task");
        let composed = match task {
            Some(task) => {
                let ceiling = if pty {
                    crate::adapter::pty::MAX_BODY
                } else {
                    store::ENQUEUE_BYTES
                };
                Some(
                    self.store
                        .compose_task_message(task, &alias, text, ceiling)?,
                )
            }
            None => None,
        };
        let text = composed.as_deref().unwrap_or(text);
        // An explicit reply_to always wins; absent one, a worker joined
        // to a group (params.upstream) reports results to its PM by
        // default. `enqueue` still validates the target.
        // A nudge owes no report: no upstream default either.
        let reply_to = optional_str(params, "reply_to")
            .map(str::to_string)
            .or_else(|| (!nudge).then(|| self.upstream_of(&alias)).flatten());
        let message = optional_str(params, "message")
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        // Caller-supplied provenance (`bootstrap` from join, etc.).
        // Identifier-charset only — internal sources like
        // `worker_result` contain characters this rejects, so the
        // internal routing contract cannot be forged through agent_send.
        let source = if nudge {
            store::NUDGE_SOURCE
        } else {
            optional_str(params, "source").unwrap_or("user")
        };
        proto::identifier(source, "Message source")?;
        // CAD-574: the operator's chat may cite the needs-me rows it
        // asks about — `{kind,id}` pairs the board renders against the
        // message. The field is `thread_send`'s alone (its own
        // allowlist): an agent or operator `send`/`ask` carrying it is
        // refused whole, never silently stripped.
        let refs = match params.get("refs") {
            None | Some(Value::Null) => None,
            Some(v) => Some(thread_refs(v)?),
        };
        let sender = sender_of(&alias)?;
        if refs.is_some() && sender != store::Sender::OperatorChat {
            return Err(Error::rejected(
                "refs is a thread_send field — only the operator's chat cites \
                 needs rows; `cadence send` and `agent_send` carry none",
            ));
        }
        let (duplicate, state) = self.store.enqueue_steered(
            &alias,
            text,
            reply_to.as_deref(),
            &message,
            source,
            task,
            None,
            None,
            &sender,
            &steer,
            refs.as_ref(),
        )?;
        // Each superseded row's `reply_to` got a notice in the same
        // transaction — wake those recipients like `message cancel` does.
        for id in &supersedes {
            if let Some(old) = self.store.message(id)? {
                if let Some(result) = old.result.as_ref() {
                    self.notify_routed_target(&old, result);
                }
            }
        }
        self.notify_agent(&alias);
        self.wake();
        let mut receipt = json!({"message": message, "state": state, "duplicate": duplicate});
        // CAD-251: an undrained mailbox warns the sender — never refuses.
        if let Some(warning) = self.inbox_warning(&alias) {
            receipt["warning"] = json!(warning);
        }
        Ok(receipt)
    }

    /// Send and wait for the message's terminal state, bounded by `wait`.
    pub(super) fn rpc_ask(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let wait = optional_u64(params, "wait").unwrap_or(120).min(600);
        let result = self.rpc_send_from(params, peer_pid)?;
        let message = result["message"].as_str().unwrap_or_default().to_string();
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let stored = self
                .store
                .message(&message)?
                .ok_or_else(|| Error::internal("Message vanished"))?;
            if is_terminal(&stored.state) || Instant::now() >= deadline {
                return Ok(stored.to_json());
            }
            self.changed
                .wait_until(Instant::now() + Duration::from_millis(250));
        }
    }

    /// The live adapter for an alias, when an actor owns one.
    pub(super) fn adapter_for(&self, alias: &str) -> Result<Arc<dyn ProviderAdapter>> {
        self.lifecycle
            .lock()
            .unwrap()
            .agents
            .get(alias)
            .and_then(|ctl| ctl.adapter.lock().unwrap().clone())
            .ok_or_else(|| {
                // A mailbox never has an adapter — name its real verb.
                if self
                    .store
                    .agent(alias)
                    .map(|a| !registry::has_actor(&a.provider, &a.endpoint_kind))
                    .unwrap_or(false)
                {
                    Error::rejected(format!(
                        "Agent '{alias}' is an inbox — no live endpoint; \
                         `cadence inbox {alias}` drains the queue"
                    ))
                } else {
                    Error::rejected("Agent has no live endpoint (not running?)")
                }
            })
    }

    /// The caller's derived identity for pane-attention verbs
    /// (`answer`), from three signals: `/proc` ancestry from the
    /// `SO_PEERCRED` pid (a pid that descends from an agent's pane root
    /// IS that agent — the one unforgeable signal), the pane's own
    /// `CADENCE_ALIAS` env the peer still carries (a `setsid` detach
    /// keeps it), and a shared pty via fd targets (detach keeps stdio).
    /// The last two are caller-choosable, which is safe here because
    /// they only narrow (see `src/peer.rs`, CAD-276). The signals are
    /// [`PeerTies`] — the one rule the
    /// board's write identity shares (CAD-263). A pane must never act
    /// on its own pane state: a worker that can reach the socket could
    /// otherwise self-sanction the very decision the menu exists to
    /// gate.
    ///
    /// Deterministic: the target's pane is checked first — self-refusal
    /// never loses to map order — then the others sorted by alias.
    /// Fails closed: the fleet map itself must load (a store error is
    /// a refusal, never an empty map that skips the self-check), and a
    /// caller whose ancestry cannot be fully walked is never stamped
    /// `operator` — while the target's pane is alive the ambiguity is
    /// a refusal, after it is gone the stamp is `unknown`. `operator`
    /// requires positive terminal evidence — the peer holding a pty
    /// that is no pane's; a fully detached caller (no ancestry hit, no
    /// env alias, no tty) matches nothing and is honestly `unknown`.
    /// Returns `(by, by_kind)`; callers record `claimed_by` separately
    /// when the supplied `by` disagrees.
    pub(super) fn derived_caller(
        &self,
        alias: &str,
        peer_pid: u32,
        verb: &str,
    ) -> Result<(String, &'static str)> {
        // Pane rows checked against their recorded start (CAD-385): a
        // reused pid is dropped — no tie, no stamp — and an unproven
        // row still narrows (the self-check) but never stamps.
        let panes = crate::peer::AgentPids::classify(self.store.pty_pane_pids()?);
        let peer = PeerTies::probe(peer_pid);
        if let Some(target) = panes.get(alias) {
            if peer.tied_to(alias, target.pid) {
                return Err(Error::rejected(format!(
                    "a pane cannot {verb} its own pane — the caller is tied \
                     to the target's pane process",
                )));
            }
        }
        if let Some(row) = panes
            .unproven()
            .find(|row| row.alias != alias && peer.tied_to(&row.alias, row.pid))
        {
            return Err(Error::rejected(format!(
                "cannot derive the caller for `{verb}`: {}",
                crate::peer::unproven_row(&row.alias, row.pid)
            )));
        }
        let live = panes.live();
        let others = live
            .iter()
            .filter(|(_, a)| a.as_str() != alias)
            .map(|(pane_pid, a)| (a.as_str(), *pane_pid));
        if let Some(agent) = peer.agents(others).into_iter().next() {
            return Ok((agent, "agent"));
        }
        // `/proc/<pid>` is a directory — `read_link` on it is always
        // EINVAL, so liveness is a `metadata` existence check.
        let target_alive = panes
            .get(alias)
            .is_some_and(|row| std::fs::metadata(format!("/proc/{}", row.pid)).is_ok());
        unmatched_caller(peer.walked(), target_alive, peer.on_tty(), verb)
    }

    /// `agent answer`: one menu-choice keystroke to a pane currently
    /// probing `approval_menu` — the adapter re-probes and refuses
    /// anything else, so the key can never land in a prompt or a
    /// running turn. Records `approval_answered` with who answered
    /// and the menu line the answer went to.
    pub(super) fn rpc_answer(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let choice = required_str(params, "choice")?;
        let claimed_by = optional_str(params, "by");
        let note = optional_str(params, "note");
        // The answerer's identity is DERIVED, never claimed — see
        // `derived_caller` for the signals and the fail-closed rule.
        let (by, by_kind) = self.derived_caller(&alias, peer_pid, "answer")?;
        let probe = self.adapter_for(&alias)?.answer_approval(choice)?;
        let mut detail = json!({
            "by": by,
            "by_kind": by_kind,
            "caller_pid": peer_pid,
            "choice": choice,
            "line": probe.reason.clone(),
            "probe": probe.to_json(),
        });
        // A supplied `by` that disagrees with the derived identity is
        // preserved as a claim, not an attribution.
        if let Some(c) = claimed_by {
            if c != by {
                detail["claimed_by"] = json!(c);
            }
        }
        if let Some(n) = note {
            detail["note"] = json!(n);
        }
        let _ = self.store.event_public(&alias, "approval_answered", detail);
        self.wake();
        // An answered menu may be exactly what a queued head waits
        // behind — wake the delivery loop rather than leaving it to
        // sit out the gate backoff.
        self.notify_agent(&alias);
        Ok(json!({"alias": alias, "state": "answered", "choice": choice}))
    }

    /// `agent recover-submit` (CAD-152): a task message that was pasted
    /// into a pty pane but never submitted — `running` under its turn
    /// token, the draft still sitting in the input line (the AOS-11
    /// lost submit) — is submitted with exactly one Enter, never a
    /// re-paste, so its turn token and report path stay the ones minted
    /// at paste.
    ///
    /// Caller rule (CAD-149): only the operator or the target's own PM
    /// ([`crate::peer::may_mutate_agent`], `Controlled`) — a pane must
    /// never submit its own input, and a peer never another's.
    ///
    /// Every precondition holds at action time or nothing is sent:
    /// the message checks ([`Self::recover_message_check`]) run first
    /// and again inside the adapter's critical section just before the
    /// Enter, with the agent re-read (same generation, same live
    /// adapter); the adapter checks the live generation, the pane, the
    /// probe and the draft ([`ProviderAdapter::recover_submit`]). Each
    /// refusal names its check. Recoveries are serialised daemon-wide.
    /// The `submit_recovered` record is reserved (result `sending`)
    /// together with the report-clock restart in one store write before
    /// the Enter, then completed with the after probe and result
    /// (`submitted` / `unconfirmed`); `submit_recover_refused` records an
    /// authorized caller's refusal. Both carry the caller, alias,
    /// generation, message id, before/after probe and result — never the
    /// message body, the draft or the turn token.
    pub(super) fn rpc_recover_submit(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        const VERB: &str = "agent recover-submit";
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let id = required_str(params, "message")?.to_string();
        let inspected = optional_str(params, "generation").map(str::to_string);
        let agent = self.store.agent(&alias)?;
        let caller = self.authorize_agent_mutation(
            params,
            peer_pid,
            VERB,
            &agent,
            AgentMutation::Controlled,
        )?;
        let _serial = self.recover_lock.lock().unwrap_or_else(|e| e.into_inner());
        // Re-read after the wait: a recovery that ran first may have
        // changed what this one sees.
        let agent = self.store.agent(&alias)?;
        let generation = agent.generation.clone().unwrap_or_default();
        let mut audit = caller_audit(&caller);
        audit["caller_pid"] = json!(peer_pid);
        audit["alias"] = json!(alias);
        audit["message"] = json!(id);
        audit["generation"] = json!(generation);
        let refuse = |check: &str, reason: &str, probe: Option<&Probe>| -> Result<Value> {
            let mut detail = audit.clone();
            detail["check"] = json!(check);
            detail["reason"] = json!(reason);
            detail["before"] = probe.map_or(Value::Null, Probe::to_json);
            detail["after"] = Value::Null;
            detail["result"] = json!("refused");
            let _ = self
                .store
                .event_public(&alias, "submit_recover_refused", detail);
            Err(Error::rejected(format!(
                "{VERB} refused ({check}): {reason}"
            )))
        };
        if agent.endpoint_kind != "pty" {
            return refuse(
                "unsupported_endpoint",
                &format!(
                    "agent '{alias}' is a {} endpoint — only a pty pane has a staged \
                     draft to submit",
                    agent.endpoint_kind
                ),
                None,
            );
        }
        let check = || self.recover_message_check(&agent, &id, inspected.as_deref());
        let message = match check() {
            Ok(message) => message,
            Err((c, reason)) => return refuse(&c, &reason, None),
        };
        let adapter = match self.adapter_for(&alias) {
            Ok(adapter) => adapter,
            Err(e) => return refuse("endpoint", &e.to_string(), None),
        };
        // Inside the adapter's critical section, just before the Enter:
        // re-read the agent (its generation and live adapter must be the
        // ones this recovery started on), re-run the message checks, and
        // durably reserve the recovery — the `submit_recovered` record
        // (result `sending`) and the report-clock restart land in one
        // store write BEFORE the key, so no later failure can admit a
        // second Enter.
        let reserved: std::cell::Cell<Option<i64>> = std::cell::Cell::new(None);
        let confirm = |before: &Probe| -> std::result::Result<(), (String, String)> {
            let fail = |check: &str, reason: String| Err((check.to_string(), reason));
            let now = match self.store.agent(&alias) {
                Ok(now) => now,
                Err(e) => return fail("message", format!("agent lookup failed: {e}")),
            };
            if now.generation.as_deref() != Some(generation.as_str()) {
                return fail(
                    "stale_generation",
                    "the endpoint was relaunched during the recovery".to_string(),
                );
            }
            let same_adapter = self
                .adapter_for(&alias)
                .is_ok_and(|live| std::ptr::addr_eq(Arc::as_ptr(&live), Arc::as_ptr(&adapter)));
            if !same_adapter {
                return fail(
                    "endpoint",
                    "the agent's endpoint was replaced during the recovery".to_string(),
                );
            }
            self.recover_message_check(&now, &id, inspected.as_deref())?;
            let mut detail = audit.clone();
            detail["before"] = before.to_json();
            detail["after"] = Value::Null;
            detail["result"] = json!("sending");
            match self.store.reserve_submit_recovery(&alias, &id, detail) {
                Ok(Some(seq)) => {
                    reserved.set(Some(seq));
                    Ok(())
                }
                Ok(None) => fail(
                    "already_submitted",
                    format!("recover-submit already sent its Enter for message {id}"),
                ),
                Err(e) => fail(
                    "record",
                    format!("the recovery could not be recorded before sending ({e})"),
                ),
            }
        };
        let outcome = adapter.recover_submit(&generation, &message.body, &confirm);
        let (before, after, confirmed, send_error) = match outcome {
            Ok(adapter::RecoverSubmit::Sent {
                before,
                after,
                confirmed,
                send_error,
            }) => (before, after, confirmed, send_error),
            Ok(adapter::RecoverSubmit::Refused {
                check,
                reason,
                probe,
            }) => return refuse(&check, &reason, probe.as_ref()),
            // Nothing was sent: every failure before the Enter is a
            // refusal of this action, recorded like one.
            Err(e) => return refuse("endpoint", &format!("pane read failed: {e}"), None),
        };
        let result = if confirmed {
            "submitted"
        } else {
            "unconfirmed"
        };
        let mut outcome = json!({"after": after.to_json(), "result": result});
        if let Some(e) = &send_error {
            outcome["send_error"] = json!(e);
        }
        // The reservation already stands — a failed outcome write leaves
        // `sending` on record (a second recovery still refuses) and is
        // reported as uncertain, never as a silent success.
        let seq = reserved.get().ok_or_else(|| {
            Error::internal(format!("{VERB}: an Enter was sent without its reservation"))
        })?;
        self.store
            .finish_submit_recovery(seq, &outcome)
            .map_err(|e| {
                Error::unknown(format!(
                    "{VERB}: the Enter was sent but its outcome could not be recorded \
                     ({e}) — inspect with `cadence agent capture {alias}`"
                ))
            })?;
        // The turn really starts now: the stall watch's clock and the
        // delivery loop should see it.
        self.bump_activity(&alias);
        self.notify_agent(&alias);
        self.wake();
        let mut out = json!({
            "alias": alias,
            "message": id,
            "state": result,
            "generation": generation,
            "before": before.to_json(),
            "after": after.to_json(),
        });
        if !confirmed {
            out["note"] = json!(
                "one Enter was sent but the TUI's empty prompt line did not come back \
                 within the bound — the outcome is unconfirmed and is never retried; \
                 inspect with `cadence agent capture`"
            );
        }
        Ok(out)
    }

    /// The message-side preconditions of `agent recover-submit` —
    /// `Err((check, reason))` names the first that fails, and no reason
    /// quotes the body. Run before the adapter is touched and again
    /// inside its critical section, just before the Enter.
    fn recover_message_check(
        &self,
        agent: &Agent,
        id: &str,
        inspected: Option<&str>,
    ) -> std::result::Result<Message, (String, String)> {
        let fail = |check: &str, reason: String| Err((check.to_string(), reason));
        let alias = agent.alias.as_str();
        let message = match self.store.message(id) {
            Ok(Some(m)) if m.alias == alias => m,
            Ok(_) => return fail("message", format!("agent '{alias}' has no message '{id}'")),
            Err(e) => return fail("message", format!("message lookup failed: {e}")),
        };
        match self.store.submit_recovered(alias, id) {
            Ok(None) => {}
            Ok(Some(prior)) => {
                return fail(
                    "already_submitted",
                    format!(
                        "recover-submit already sent its Enter for message {id} (result: {}, \
                         by {}) — it is never sent twice",
                        prior["result"].as_str().unwrap_or("unknown"),
                        prior["by"].as_str().unwrap_or("unknown"),
                    ),
                )
            }
            Err(e) => return fail("message", format!("recovery record lookup failed: {e}")),
        }
        let held = match self.store.held_turns(alias) {
            Ok(held) => held,
            Err(e) => return fail("message", format!("held-turn lookup failed: {e}")),
        };
        if let Some(other) = held.iter().find(|m| m.id != id) {
            return fail(
                "other_message_pending",
                format!(
                    "message {id} is {} — the pasted message agent '{alias}' holds is {}",
                    message.state, other.id
                ),
            );
        }
        if message.state != "running" {
            return fail(
                "not_running",
                format!(
                    "message {id} is {} — only a pasted, unreported turn (running) can \
                     be recovered",
                    message.state
                ),
            );
        }
        if !message.holds_turn() {
            return fail(
                "not_a_turn",
                format!(
                    "message {id} is a routed notice or nudge — it completes at paste, \
                     there is no turn to recover"
                ),
            );
        }
        let acked = message
            .result
            .as_ref()
            .is_some_and(|r| r.get("ack").is_some_and(|a| !a.is_null()));
        if acked {
            return fail(
                "acknowledged",
                format!("agent '{alias}' already acknowledged message {id} — it was submitted"),
            );
        }
        let Some(generation) = agent.generation.as_deref() else {
            return fail(
                "stale_generation",
                format!("agent '{alias}' has no live endpoint generation"),
            );
        };
        if inspected.is_some_and(|g| g != generation) {
            return fail(
                "stale_generation",
                "the endpoint generation changed since it was inspected".to_string(),
            );
        }
        let current = message.turn_id.as_deref().is_some_and(|token| {
            registry::turn_token_current(
                &agent.provider,
                &agent.endpoint_kind,
                Some(generation),
                token,
            )
        });
        if !current {
            return fail(
                "stale_generation",
                format!(
                    "message {id} was pasted under an earlier endpoint generation — its \
                     turn token is not the live endpoint's"
                ),
            );
        }
        Ok(message)
    }

    /// Explicit ack/result report for a running message. The `token` is
    /// the `turn_id` minted at submission; it embeds the endpoint
    /// generation under the endpoint's own scheme (CAD-162:
    /// `registry::turn_token_current`), so a report aimed at a previous
    /// endpoint life — or carrying another endpoint kind's token — is
    /// rejected as stale, and an endpoint with no checkable scheme
    /// refuses every report. Callers are identified by possession of
    /// the token, which is self-asserted — not an authentication.
    ///
    /// On an endpoint whose adapter turn result completes the message
    /// (managed), only `ack` is accepted: the turn result is the one
    /// writer of the outcome, so a reported `result` would race it.
    pub(super) fn rpc_message_report(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "message")?;
        let token = required_str(params, "token")?;
        let kind = required_str(params, "kind")?;
        let text = optional_str(params, "text");
        let message = self
            .store
            .message(id)?
            .ok_or_else(|| Error::rejected("Unknown message"))?;
        let agent = self.store.agent(&message.alias)?;
        if message.turn_id.as_deref() != Some(token) {
            return Err(Error::rejected(
                "Token does not match the message's submission token",
            ));
        }
        if !registry::turn_token_current(
            &agent.provider,
            &agent.endpoint_kind,
            agent.generation.as_deref(),
            token,
        ) {
            return Err(Error::rejected(
                "Submission token belongs to a stale endpoint generation",
            ));
        }
        if kind == "result" && registry::reports_turn_result(&agent.provider, &agent.endpoint_kind)
        {
            return Err(Error::rejected(
                "This endpoint's turn result completes the message — report `ack` only",
            ));
        }
        // CAD-341: `check` runs the token/generation/state gates of a
        // result report and changes nothing — `message result --report`
        // files its report only once the daemon would take the result.
        // It also names the board issue the message's task is bound to.
        if params
            .get("check")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if kind != "result" {
                return Err(Error::rejected("`check` applies to result reports only"));
            }
            if !matches!(message.state.as_str(), "running" | "completed") {
                return Err(Error::rejected(format!(
                    "Message is not awaiting a report (state {})",
                    message.state
                )));
            }
            let issue = message
                .task_id
                .as_deref()
                .and_then(|t| self.store.task(t).ok())
                .and_then(|t| self.store.job(&t.job_id).ok())
                .and_then(|j| j.issue_id);
            let result_text = message.result.as_ref().and_then(|r| r.get("text")).cloned();
            return Ok(json!({"state": message.state, "task": message.task_id,
                             "issue": issue, "result_text": result_text}));
        }
        // A valid ack/result report is explicit agent activity — it
        // feeds the stall clock for every endpoint kind.
        self.bump_activity(&message.alias);
        match kind {
            "ack" => {
                self.store.mark_ack(&message, text)?;
            }
            "result" => {
                let text = text.ok_or_else(|| Error::rejected("A result report requires text"))?;
                // `--sha` binds the report to an exact commit — the
                // verdict protocol requires it on task-attached work.
                let sha = optional_str(params, "sha")
                    .map(store::check_commit_sha)
                    .transpose()?;
                // CAD-250 F1: the `running` check and the finish are one
                // transaction — a report racing the report bound (or a
                // reconcile) either wins outright or is judged against
                // the row as it now stands, never both.
                let message = if message.state == "running" {
                    let stored = json!({
                        "status": "completed", "text": text,
                        "turn_id": token, "via": "pty_report",
                        "sha": sha,
                    });
                    match self
                        .store
                        .finish_running(&message.id, "completed", &stored, None)?
                    {
                        Ok(finished) => {
                            self.notify_routed_target(&finished, &stored);
                            // The report frees the actor's one turn — wake
                            // it so the next queued delivery is claimed
                            // now, not on the idle poll.
                            self.notify_agent(&finished.alias);
                            self.wake();
                            return Ok(json!({"state": "reported", "kind": kind}));
                        }
                        Err(current) => {
                            current.ok_or_else(|| Error::rejected("Unknown message"))?
                        }
                    }
                } else {
                    message
                };
                if message.state == "completed" {
                    // Idempotent retry vs conflicting duplicate.
                    let same = message
                        .result
                        .as_ref()
                        .and_then(|r| r.get("text"))
                        .and_then(Value::as_str)
                        == Some(text);
                    if !same {
                        return Err(Error::rejected(
                            "Message already completed with a different result",
                        ));
                    }
                    return Ok(json!({"state": "completed", "duplicate": true}));
                } else {
                    return Err(Error::rejected(format!(
                        "Message is not awaiting a report (state {})",
                        message.state
                    )));
                }
            }
            other => {
                return Err(Error::rejected(format!(
                    "Report kind must be ack or result, not '{other}'"
                )))
            }
        }
        self.wake();
        Ok(json!({"state": "reported", "kind": kind}))
    }

    /// Operator reconcile of an `unknown` message — no turn token, the
    /// token is stale by definition when a message is `unknown`. The
    /// store transaction enforces unknown-only; `completed`/`failed`
    /// route `reply_to`, `interrupted` routes nothing.
    ///
    /// Operator only, by the connection (CAD-374): settling an outcome
    /// nobody could prove routes a result upstream as real, so an
    /// agent — the fenced one, a peer or its PM — is refused, and `by`
    /// is refused rather than read: the record says `operator`, the one
    /// caller this accepts.
    pub(super) fn rpc_reconcile(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("message reconcile", params, peer_pid)?;
        self.reconcile_message(params)
    }

    /// The reconcile itself, once [`Self::rpc_reconcile`]'s gate passed.
    pub(super) fn reconcile_message(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let message_id = required_str(params, "message")?;
        let status = required_str(params, "status")?;
        let note = optional_str(params, "note");
        let by = "operator";
        // An operator-stated SHA on a `completed` reconcile is bound
        // like a worker's `--sha` — explicit, never inferred.
        let sha = optional_str(params, "sha");
        let message = self.store.reconcile(message_id, status, note, by, sha)?;
        if let Some(result) = message.result.clone() {
            self.notify_routed_target(&message, &result);
        }
        // A live actor holding this cloud turn stops polling it now.
        self.notify_agent(&message.alias);
        self.wake();
        Ok(json!({"state": "reconciled", "message": message.to_json()}))
    }

    /// Cancel a still-`queued` message — never delivered. The store's
    /// state-guarded UPDATE makes the cancel atomic against an actor's
    /// `take_queued` claim; a `reply_to` gets one `worker_notice` so a
    /// waiter isn't left hanging. `wake()` so a pending ask waiter sees
    /// the terminal state promptly.
    pub(super) fn rpc_cancel(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let message_id = required_str(params, "message")?;
        let by = required_str(params, "by")?;
        let reason = optional_str(params, "reason");
        let message = self.store.cancel(message_id, by, reason)?;
        if let Some(result) = message.result.clone() {
            self.notify_routed_target(&message, &result);
        }
        self.wake();
        Ok(json!({"state": "cancelled", "message": message.to_json()}))
    }

    /// `interrupt` (CAD-323) — stop the agent's running provider turn with
    /// the provider's own interrupt ([`ProviderAdapter::interrupt_turn`]:
    /// Claude's stream-json interrupt control request, Codex
    /// `turn/interrupt`, a pty's interrupt keys), never a kill.
    ///
    /// Caller rule: the operator, or the agent's own dispatcher — its PM,
    /// via the one agent-mutation policy ([`crate::peer::may_mutate_agent`],
    /// `Controlled`), or the master for a turn it dispatched
    /// (`master_dispatched`) — an agent interrupts neither a peer nor
    /// itself.
    ///
    /// Reconciliation is the ordinary turn path: a managed turn's own
    /// result finishes the message `interrupted` (held text flushed and
    /// partial tool results recorded first, CAD-320), which leaves the
    /// agent idle for its next message; nothing is requeued, so no
    /// kickoff is replayed. A pane has no result wire, so its running
    /// message is finished `interrupted` here, guarded against a report
    /// that lands first. No running turn — or a turn that ended before
    /// the interrupt reached it — is a recorded no-op. `wait` (seconds,
    /// default 30, max 120) bounds how long the answer waits for the
    /// message to settle.
    pub(super) fn rpc_interrupt(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let agent = self.store.agent(&alias)?;
        // Read once: the turn the caller is authorized for is the one
        // interrupted — a later turn is never reached by this call.
        let running = self.store.running_message(&alias)?;
        let caller = if self.caller_is_master(peer_pid) {
            // The master dispatches tickets but is no agent's PM: it may
            // interrupt exactly a turn the daemon's own record says it
            // dispatched (`master_dispatched`, written only for a real
            // send) AND whose results route back to it (`reply_to:
            // master`) — both, so neither record alone grants it.
            reject_identity_fields(params, "interrupt")?;
            let dispatched = match &running {
                Some(m) => {
                    m.reply_to.as_deref() == Some(crate::master::ALIAS)
                        && self.store.event_names_message(
                            DAEMON_ALIAS,
                            "master_dispatched",
                            &m.id,
                        )?
                }
                None => false,
            };
            if !dispatched {
                return Err(Error::invalid(
                    "master_refused",
                    format!(
                        "the master interrupts only a turn it dispatched — {alias} is \
                         not running one; ask the operator"
                    ),
                ));
            }
            AgentCaller::Agent(crate::master::ALIAS.to_string())
        } else {
            self.authorize_agent_mutation(
                params,
                peer_pid,
                "interrupt",
                &agent,
                AgentMutation::Controlled,
            )?
        };
        let wait = optional_u64(params, "wait").unwrap_or(30).min(120);
        let audit = caller_audit(&caller);
        let noop = |reason: &str, message: Option<&Message>| -> Result<Value> {
            let mut payload = json!({"outcome": "noop", "reason": reason,
                                     "message": message.map(|m| m.id.clone())});
            payload["by"] = audit["by"].clone();
            payload["by_kind"] = audit["by_kind"].clone();
            self.store
                .event_public(&alias, "interrupt_requested", payload)?;
            Ok(
                json!({"alias": alias, "interrupted": false, "reason": reason,
                      "message": message.map(|m| m.id.clone()),
                      "state": message.map(|m| m.state.clone())}),
            )
        };
        let Some(message) = running else {
            return noop("no running turn", None);
        };
        let Some(turn_id) = message.turn_id.clone() else {
            return noop("no running turn", None);
        };
        // Test seam: widen the gap between reading the running turn and
        // reaching the adapter, so a suite can land the next turn inside
        // it. Set only on an in-process test daemon (`ProviderEnv::own`
        // never reads the process environment).
        if let Some(ms) = self
            .provider_env
            .own("CADENCE_TEST_INTERRUPT_PAUSE_MS")
            .and_then(|v| v.parse::<u64>().ok())
        {
            let _ = self.store.event_public(
                &alias,
                "interrupt_paused",
                json!({"message": message.id, "ms": ms}),
            );
            thread::sleep(Duration::from_millis(ms));
        }
        // Every call past the caller rule leaves one `interrupt_requested`
        // — a refusal included (PROTOCOL.md).
        let record = |outcome: &str, error: Option<&str>| -> Result<()> {
            let mut payload = json!({"outcome": outcome, "message": message.id,
                                     "turn_id": turn_id});
            payload["by"] = audit["by"].clone();
            payload["by_kind"] = audit["by_kind"].clone();
            if let Some(error) = error {
                payload["error"] = json!(error);
            }
            self.store
                .event_public(&alias, "interrupt_requested", payload)
        };
        let adapter = self
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .get(&alias)
            .and_then(|ctl| ctl.adapter.lock().unwrap().clone());
        let Some(adapter) = adapter else {
            let error = format!(
                "Agent '{alias}' has no live endpoint to interrupt — its running \
                 message {} is the reconcile's (`cadence message reconcile {} \
                 --status interrupted`)",
                message.id, message.id
            );
            record("refused", Some(&error))?;
            return Err(Error::rejected(error));
        };
        // The pane's settle: the guarded `interrupted` finish, run by the
        // adapter under its paste lock before any key is sent. A managed
        // endpoint never calls it — its turn result finishes the message.
        let stored = json!({"status": "interrupted", "text": "", "turn_id": turn_id,
                            "via": "interrupt", "by": audit["by"]});
        let reason = format!(
            "interrupted by {}",
            audit["by"].as_str().unwrap_or("operator")
        );
        let finished: std::cell::RefCell<Option<Message>> = std::cell::RefCell::new(None);
        let settle = || -> Result<bool> {
            match self
                .store
                .finish_running(&message.id, "interrupted", &stored, Some(&reason))?
            {
                Ok(done) => {
                    *finished.borrow_mut() = Some(done);
                    Ok(true)
                }
                Err(_) => Ok(false),
            }
        };
        let outcome = match adapter.interrupt_turn(&turn_id, &settle) {
            Ok(outcome) => outcome,
            // The provider refused — most often because the turn ended
            // on its own in the gap. A settled message is a no-op; a
            // still-running one surfaces the provider's answer.
            Err(error) => {
                let current = self.store.message(&message.id)?;
                if current.as_ref().is_some_and(|m| m.state != "running") {
                    return noop("turn already ended", current.as_ref());
                }
                record("refused", Some(&error.to_string()))?;
                return Err(error);
            }
        };
        if outcome == InterruptOutcome::NotRunning {
            let current = self.store.message(&message.id)?;
            return noop("turn already ended", current.as_ref().or(Some(&message)));
        }
        record("delivered", None)?;
        if let Some(done) = finished.into_inner() {
            self.notify_routed_target(&done, &stored);
            self.wake();
        }
        let deadline = Instant::now() + Duration::from_secs(wait);
        let settled = loop {
            let current = self.store.message(&message.id)?;
            let running = current.as_ref().is_some_and(|m| m.state == "running");
            if !running || Instant::now() >= deadline || self.closing.load(Ordering::SeqCst) {
                break current;
            }
            self.changed
                .wait_until(deadline.min(Instant::now() + Duration::from_millis(250)));
        };
        Ok(json!({
            "alias": alias,
            "interrupted": true,
            "message": message.id,
            "turn_id": turn_id,
            "state": settled.map(|m| m.state),
        }))
    }
}
