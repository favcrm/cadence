//! CAD-534: `cadence daemon` requests RPC handlers — moved verbatim from src/daemon.rs.

use super::*;

impl Shared {
    /// Who may answer an agent's pending provider or brokered request
    /// (CAD-370), from the connection: the proven operator, or the
    /// requester's own PM (`params.upstream`, bound to the PM's
    /// registration — docs/SESSION.md: "approvals come to you"). The
    /// requesting agent itself never answers its own request — that
    /// would defeat the broker — and neither does a peer or another
    /// group's PM. Identity fields are refused, not read.
    fn authorize_respond(&self, params: &Value, peer_pid: u32, requester: &str) -> Result<()> {
        reject_identity_fields(params, "agent respond")?;
        let caller = self.agent_caller(peer_pid, "agent respond")?;
        let AgentCaller::Agent(alias) = caller else {
            return Ok(());
        };
        let target = self.store.agent(requester)?;
        let pm = self.effective_pm(&target)?;
        if alias != requester && pm.as_deref() == Some(alias.as_str()) {
            return Ok(());
        }
        let owner = match pm.filter(|pm| pm != requester) {
            Some(pm) => format!("the operator or its PM '{pm}'"),
            None => "the operator (it has no PM)".to_string(),
        };
        let who = if alias == requester {
            format!("agent '{alias}' cannot answer its own request")
        } else {
            format!("agent '{alias}' cannot answer another agent's request")
        };
        Err(Error::rejected(format!(
            "agent respond refused: {who} — '{requester}''s pending requests are \
             answered only by {owner} (caller rule, CAD-370)"
        )))
    }

    pub(super) fn rpc_respond(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        // Decided before the handle is claimed, so a refused caller
        // consumes nothing and the request stays pending.
        self.authorize_respond(params, peer_pid, &alias)?;
        let handle = required_str(params, "request")?;
        let decision = optional_str(params, "decision");
        // Explicit JSON null means "not provided".
        let answers = params.get("answers").filter(|a| !a.is_null()).cloned();
        // Operator note carried on a brokered decline — the MCP server
        // hands it to the provider as the denial message.
        let reason = optional_str(params, "reason").map(str::to_string);
        // CAD-506: a `kind:"effect"` handle names a durable
        // pending-effect row, not a `pending` entry — the press path
        // decides it (accept is operator-only) and runs the staged call.
        if let Some(row) = self.store.effect_by_request(handle)? {
            return self.respond_effect(&row, peer_pid, decision, &answers, reason, &alias);
        }
        // Claim the handle atomically: whichever path removes it first
        // — this respond, an external `serverRequest/resolved`, or the
        // actor's exit sweep — owns the answer, and every other path
        // sees "no longer pending". Validation runs under the same
        // lock so a malformed respond leaves the request pending
        // instead of consuming it. No I/O happens while the lock is
        // held. A miss falls to the spec's rejection hint where one is
        // configured (managed claude without --broker-approvals).
        enum Claim {
            /// A provider-originated request: answer over the adapter.
            Provider(Value, Value),
            /// A `request_open` brokered request: the answer is parked
            /// for the blocked `request_wait` caller instead.
            Brokered,
        }
        let claim = {
            let mut map = self.pending.lock().unwrap();
            let req = map.get(handle).filter(|req| req.alias == alias);
            let Some(req) = req else {
                drop(map);
                let agent = self.store.agent(&alias)?;
                return Err(Error::rejected(
                    registry::respond_rejection(&agent.provider, &agent.endpoint_kind)
                        .unwrap_or("Request is no longer pending for this agent"),
                ));
            };
            let method = req.method.clone();
            if method.starts_with("cadence/") {
                let answer = match decision {
                    Some("accept") if answers.is_none() => json!({"decision": "accept"}),
                    Some("decline") if answers.is_none() => {
                        json!({"decision": "decline", "reason": reason})
                    }
                    _ => return Err(Error::rejected("Respond with decision accept or decline")),
                };
                // Park the answer before dropping the pending entry —
                // `request_wait` treats a missing handle as closed, so
                // the mailbox must be filled first or an accept could
                // surface as a denial.
                self.answered
                    .lock()
                    .unwrap()
                    .insert(handle.to_string(), (alias.clone(), answer));
                map.remove(handle);
                Claim::Brokered
            } else {
                let request_id = req.id.clone();
                let request_params = req.params.clone();
                let response = match method.as_str() {
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    match decision {
                        Some("accept") | Some("decline") if answers.is_none() => {
                            json!({"decision": decision.unwrap()})
                        }
                        _ => return Err(Error::rejected("Respond with decision accept or decline")),
                    }
                }
                "item/tool/requestUserInput" => {
                    if decision.is_some() || !answers.as_ref().is_some_and(Value::is_object) {
                        return Err(Error::rejected("Respond with an answers object"));
                    }
                    json!({"answers": answers.unwrap()})
                }
                "devin/user_input" => {
                    let from_answers = answers.as_ref().and_then(|value| {
                        value
                            .get("message")
                            .or_else(|| value.get("text"))
                            .and_then(Value::as_str)
                    });
                    let text = from_answers
                        .filter(|text| !text.trim().is_empty())
                        .or_else(|| decision.filter(|text| !text.trim().is_empty()))
                        .ok_or_else(|| {
                            Error::rejected(
                                "Respond with answers.message, answers.text, or --decision text for the Devin session",
                            )
                        })?;
                    json!({"message": text})
                }
                "session/request_permission" => match decision {
                    Some("decline") if answers.is_none() => {
                        json!({"outcome": {"outcome": "cancelled"}})
                    }
                    Some("accept") if answers.is_none() => {
                        let option = request_params
                            .get("options")
                            .and_then(Value::as_array)
                            .and_then(|options| {
                                options.iter().find(|o| {
                                    o.get("kind").and_then(Value::as_str) == Some("allow_once")
                                })
                            })
                            .ok_or_else(|| {
                                Error::rejected("Provider did not offer an allow-once option")
                            })?;
                        json!({"outcome": {"outcome": "selected", "optionId": option["optionId"]}})
                    }
                    _ => return Err(Error::rejected("Respond with decision accept or decline")),
                },
                _ => return Err(Error::rejected(
                    "This request type is not supported; stop the agent or use the provider directly",
                )),
                };
                map.remove(handle);
                Claim::Provider(request_id, response)
            }
        };
        match claim {
            Claim::Brokered => {}
            Claim::Provider(request_id, response) => {
                let adapter = self
                    .lifecycle
                    .lock()
                    .unwrap()
                    .agents
                    .get(&alias)
                    .and_then(|ctl| ctl.adapter.lock().unwrap().clone());
                let adapter = adapter.ok_or_else(|| {
                    Error::internal("Agent adapter is not available for this request")
                })?;
                adapter.respond(&request_id, response)?;
            }
        }
        self.relax_waiting(&alias);
        let _ = self
            .store
            .event_public(&alias, "input_answered", json!({"request": handle}));
        self.wake();
        Ok(json!({"state": "answered"}))
    }

    /// The agent a brokered-request RPC comes from (CAD-376), from the
    /// connection alone: the nearest registered pane or strictly
    /// verified enrolled endpoint on the peer's ancestry
    /// ([`Self::slot_identity`]) — never the request's `alias`, never
    /// `CADENCE_ALIAS`. A brokered handle belongs to its agent's own
    /// permission server (`cadence mcp-permission`, a child of the
    /// brokered provider), so only that agent opens, waits on or
    /// closes it. A connection with no agent identity is refused too:
    /// the operator (and the requester's PM) answer with `agent
    /// respond`, which is gated separately (CAD-370).
    pub(super) fn request_caller(
        &self,
        params: &Value,
        peer_pid: u32,
        verb: &str,
    ) -> Result<String> {
        reject_identity_fields(params, verb)?;
        self.revalidate_enrollments()?;
        match self.slot_identity(peer_pid)? {
            Some(who) if !who.lane().is_empty() => Ok(who.lane().to_string()),
            Some(_) => Err(Error::rejected(format!(
                "{verb} refused: caller pid {peer_pid} descends from a pane whose \
                 agent cannot be named — caller identity underivable"
            ))),
            None => Err(Error::rejected(format!(
                "{verb} refused: this connection derives no agent identity — a \
                 brokered request is opened, awaited and closed only by its agent's \
                 own permission server; the operator or the agent's PM answers it \
                 with `cadence agent respond` (caller rule, CAD-376)"
            ))),
        }
    }

    /// The refusal for a caller naming another agent's brokered request.
    pub(super) fn foreign_request(verb: &str, caller: &str, owner: &str) -> Error {
        Error::rejected(format!(
            "{verb} refused: agent '{caller}' cannot act on '{owner}''s brokered \
             requests — only '{owner}''s own permission server opens, awaits and \
             closes them (caller rule, CAD-376)"
        ))
    }

    /// `request_open` — a brokered request raised by an external
    /// requester (the `cadence mcp-permission` server a brokered
    /// claude launches) rather than by the provider adapter itself.
    /// Same model as `on_provider_request`: durable through the event
    /// log, visible via `agent_requests`, and holding the agent in
    /// `waiting_input` until `agent respond` answers it. Only the
    /// agent's own connection opens one ([`Self::request_caller`]).
    pub(super) fn rpc_request_open(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        // Decided before anything is recorded, so a refused caller
        // parks nothing and notifies no one.
        let caller = self.request_caller(params, peer_pid, "request_open")?;
        if caller != alias {
            return Err(Self::foreign_request("request_open", &caller, &alias));
        }
        let agent = self.store.agent(&alias)?;
        let brokered = agent
            .params
            .as_ref()
            .and_then(|p| p.get("broker_approvals"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !brokered {
            return Err(Error::rejected(format!(
                "Agent '{alias}' was not launched with --broker-approvals — \
                 its permission prompts are not brokered"
            )));
        }
        let kind = optional_str(params, "kind").unwrap_or("approval");
        proto::identifier(kind, "Request kind")?;
        let tool = required_str(params, "tool")?;
        let input_summary = optional_str(params, "input_summary").unwrap_or_default();
        let input = params.get("input").cloned().unwrap_or(Value::Null);
        // The caller may name the handle (the mcp-permission server
        // mints one per tool call): a retry after a lost response then
        // re-opens the SAME request — no duplicate pending entry, no
        // second event, no second PM notice.
        let handle = match optional_str(params, "request") {
            Some(h) => proto::identifier(h, "Request handle")?,
            None => Uuid::new_v4().simple().to_string(),
        };
        // A durable pending effect already owns this handle — its
        // request is never shadowed by a brokered one (CAD-506: one
        // handle = one request = one execution).
        if self.store.effect_by_request(&handle)?.is_some() {
            return Err(Error::rejected(format!(
                "request_open refused: handle '{handle}' names a staged platform \
                 effect — a handle belongs to one request (CAD-506)"
            )));
        }
        // A handle names one agent's request until that agent's wait
        // collects the answer (CAD-452). `agent respond` parks the
        // answer and drops the pending entry, so the mailbox is checked
        // too: a peer re-opening an answered handle under its own alias
        // would own it, the owner's wait would be refused, and the
        // operator's accept would reach the provider as a deny. Check
        // and insert share one scope, locked in respond's order
        // (pending → answered), so no respond or open lands between.
        {
            let mut pending = self.pending.lock().unwrap();
            let answered = self.answered.lock().unwrap();
            let held = pending
                .get(&handle)
                .map(|req| (&req.alias, "waiting_input", "already pending for"))
                .or_else(|| {
                    answered
                        .get(&handle)
                        .map(|(owner, _)| (owner, "answered", "holding an answer parked for"))
                });
            if let Some((owner, state, what)) = held {
                if *owner == alias {
                    // The owner's retry: its wait collects any answer.
                    return Ok(json!({"request": handle, "state": state, "existing": true}));
                }
                return Err(Error::rejected(format!(
                    "request_open refused: handle '{handle}' is {what} '{owner}' — a \
                     handle names one agent's request until that agent's wait collects \
                     its answer (CAD-452)"
                )));
            }
            drop(answered);
            pending.insert(
                handle.clone(),
                PendingRequest {
                    alias: alias.clone(),
                    // No provider request id — the answer parks in
                    // `answered` for `request_wait`, never `adapter.respond`.
                    id: Value::Null,
                    method: format!("cadence/{kind}"),
                    params: json!({"kind": kind, "tool": tool,
                               "input_summary": input_summary, "input": input}),
                },
            );
        }
        // Requests only arrive mid-turn; relax/stop may have moved the
        // agent on already — never clobber a non-busy state.
        let _ = self
            .store
            .set_agent_state_if(&alias, "waiting_input", "busy");
        // `agent_events` is an unscoped `Rule::Read` — a peer (or any
        // unattributed caller) reads another agent's lane, so the open
        // event carries routing fields only. The input-derived text
        // stays on the pending row (`agent_requests` discloses it to
        // the operator, the owner and the owner's PM) and on the PM
        // notice below (CAD-542; the CAD-506 fix for platform effects,
        // applied to brokered approvals).
        let _ = self.store.event_public(
            &alias,
            "request_opened",
            json!({"request": handle, "kind": kind, "tool": tool}),
        );
        // An upstream PM gets exactly one notice naming the agent and
        // the respond command — deterministic id, so a retried open
        // can never double-notify.
        if let Some(pm) = self.upstream_of(&alias) {
            let delivery = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("cadence-notice:request:{handle}").as_bytes(),
            )
            .simple()
            .to_string();
            let payload = json!({"request": handle, "worker": alias,
                                 "tool": tool, "input_summary": input_summary});
            let body = format!(
                "A managed worker is waiting on a tool-permission decision. This is an \
                 informational notice, not a result; do not treat it as worker output. \
                 Answer it with `cadence agent respond {alias} --request {handle} \
                 --decision accept|decline [--reason \"why\"]` — `cadence agent requests \
                 {alias}` shows the full input. {payload}"
            );
            if self
                .store
                .enqueue_task(&pm, &body, None, &delivery, "worker_notice", None)
                .is_ok()
            {
                self.notify_agent(&pm);
            }
        }
        // An open request counts as provider activity — a worker
        // waiting on a human is not idle.
        if let Ok(adapter) = self.adapter_for(&alias) {
            adapter.note_activity();
        }
        self.wake();
        Ok(json!({"request": handle, "state": "waiting_input"}))
    }

    /// `request_wait` — block until a brokered request is answered,
    /// closed, or the caller's slice expires. `request_wait` callers
    /// re-issue until their own deadline; each pass stamps provider
    /// activity so a human's thinking time is never an idle fence.
    /// Only the owning agent's connection waits: the parked answer is
    /// consumed here, so another agent's wait could take the answer
    /// and leave the owner's server reading `closed` — a denial.
    pub(super) fn rpc_request_wait(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let handle = required_str(params, "request")?;
        let caller = self.request_caller(params, peer_pid, "request_wait")?;
        let wait = optional_u64(params, "wait").unwrap_or(60).min(120);
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let alias = self
                .pending
                .lock()
                .unwrap()
                .get(handle)
                .map(|req| req.alias.clone());
            let Some(alias) = alias else {
                // The handle is gone — `agent respond` parks the
                // answer before dropping it, so the mailbox is
                // authoritative here; an actor exit sweep or a daemon
                // restart leaves it empty, which reads as closed.
                let mut answered = self.answered.lock().unwrap();
                if let Some((owner, _)) = answered.get(handle) {
                    if *owner != caller {
                        return Err(Self::foreign_request("request_wait", &caller, owner));
                    }
                    let (_, answer) = answered.remove(handle).expect("entry just read");
                    return Ok(json!({"state": "answered", "answer": answer}));
                }
                drop(answered);
                // CAD-506: the handle may name a durable pending effect
                // — the row owns its lifecycle; this wait only polls it.
                if let Some(state) = self.effect_wait(handle, &caller)? {
                    if state["state"] == "waiting" {
                        if Instant::now() >= deadline {
                            return Ok(state);
                        }
                        self.changed
                            .wait_until(Instant::now() + Duration::from_millis(250));
                        continue;
                    }
                    return Ok(state);
                }
                return Ok(json!({"state": "closed",
                                 "reason": "request is not pending"}));
            };
            if alias != caller {
                return Err(Self::foreign_request("request_wait", &caller, &alias));
            }
            if let Ok(adapter) = self.adapter_for(&alias) {
                adapter.note_activity();
            }
            if self.closing.load(Ordering::SeqCst) {
                return Ok(json!({"state": "closed", "reason": "daemon shutting down"}));
            }
            if Instant::now() >= deadline {
                return Ok(json!({"state": "waiting"}));
            }
            self.changed
                .wait_until(Instant::now() + Duration::from_millis(250));
        }
    }

    /// `request_close` — the requester's local deadline fired: retire
    /// the pending handle so the agent leaves `waiting_input` and
    /// `agent_requests` drains. An answer parked at the boundary still
    /// lands — `agent respond` fills the mailbox before dropping the
    /// pending entry, so a close that finds it returns `answered`.
    /// Only the owning agent's connection closes a handle; a refused
    /// close leaves it pending (and any parked answer parked).
    pub(super) fn rpc_request_close(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let handle = required_str(params, "request")?;
        // Derived before the locks: it walks /proc and reads the store.
        let caller = self.request_caller(params, peer_pid, "request_close")?;
        let alias = {
            let mut pending = self.pending.lock().unwrap();
            // Same lock order as respond (pending → answered): a
            // respond mid-flight holds pending through its mailbox
            // insert, so whichever we observe here is final.
            let mut answered = self.answered.lock().unwrap();
            let owner = answered
                .get(handle)
                .map(|(alias, _)| alias)
                .or_else(|| pending.get(handle).map(|req| &req.alias));
            if let Some(owner) = owner.filter(|owner| **owner != caller) {
                return Err(Self::foreign_request("request_close", &caller, owner));
            }
            if let Some((alias, answer)) = answered.remove(handle) {
                drop(answered);
                pending.remove(handle);
                drop(pending);
                self.relax_waiting(&alias);
                return Ok(json!({"state": "answered", "answer": answer}));
            }
            drop(answered);
            pending.remove(handle).map(|req| req.alias)
        };
        if let Some(alias) = alias {
            self.relax_waiting(&alias);
            let _ = self
                .store
                .event_public(&alias, "request_closed", json!({"request": handle}));
        }
        // CAD-506 §5.4 step 7: a `kind:"effect"` handle names a durable
        // row — the caller's close ends only its wait; the pending
        // effect stays `waiting` for the press. Nothing is recorded.
        if let Some(state) = self.effect_caller_close(handle, &caller)? {
            self.wake();
            return Ok(state);
        }
        self.wake();
        Ok(json!({"state": "closed"}))
    }
}
