//! CAD-534: `cadence daemon` identity RPC handlers — moved verbatim from src/daemon.rs.

use super::*;

use crate::peer::PeerTies;

impl Shared {
    /// Revalidate every active strict enrollment against its owner row
    /// before a slot call: a missing row, a closed endpoint or a changed
    /// owner generation revokes (CAD-230). A store that cannot answer
    /// refuses the call instead — it proves no drift, so it neither
    /// revokes nor lets an unrevalidated enrollment admit.
    pub(super) fn revalidate_enrollments(&self) -> Result<()> {
        let owners = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enrolled_owners();
        if owners.is_empty() {
            return Ok(());
        }
        let mut current: HashMap<String, Option<String>> = HashMap::new();
        for alias in owners {
            let row = self.store.agent_opt(&alias)?;
            current.insert(alias, row.as_ref().and_then(owner_generation));
        }
        let events = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revalidate_owners(&current);
        self.emit_slot_events(events);
        Ok(())
    }

    /// Mint (or renew) the strict build-slot enrollment for a managed
    /// endpoint that just opened — from the pid the adapter recorded
    /// and the owner row as now written. Admission is a side benefit
    /// of the endpoint: a refusal is recorded, never fatal.
    pub(super) fn enroll_endpoint(&self, alias: &str) {
        let Ok(agent) = self.store.agent(alias) else {
            return;
        };
        if !registry::enrolls_build_slots(&agent.provider, &agent.endpoint_kind) {
            return;
        }
        let (Some(generation), Some(pid)) = (
            owner_generation(&agent),
            agent.pid.and_then(|p| u32::try_from(p).ok()),
        ) else {
            return;
        };
        // CAD-385: the enrollment roots at the process the row recorded
        // — its pid AND start time — never at whatever holds the pid now.
        let recorded = agent.pid_start.and_then(|s| u64::try_from(s).ok());
        let proof = crate::peer::pid_proof(pid, recorded);
        if proof != crate::peer::PidProof::Same {
            let _ = self.store.event_public(
                alias,
                "slot_enrollment_refused",
                json!({"pid": pid, "reason": format!(
                    "pid {pid} is not provably the recorded provider process ({proof:?} \
                     against its recorded start time)"
                )}),
            );
            return;
        }
        let clk = crate::slots::SlotClock::at((self.slot_clock)(), epoch_secs());
        let outcome = self.slots.lock().unwrap_or_else(|e| e.into_inner()).enroll(
            alias,
            &generation,
            pid,
            clk,
        );
        match outcome {
            Ok((_, events)) => self.emit_slot_events(events),
            Err(e) => {
                let _ = self.store.event_public(
                    alias,
                    "slot_enrollment_refused",
                    json!({"pid": pid, "reason": e.to_string()}),
                );
            }
        }
    }

    /// The endpoint closed: its enrollment authorizes nothing more.
    pub(super) fn revoke_endpoint(&self, alias: &str, reason: &str) {
        let events = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revoke_owner(alias, reason);
        self.emit_slot_events(events);
    }

    /// The one caller-identity verifier (CAD-381, decision #3): who is
    /// on the other end of this Unix connection, answered only from the
    /// daemon's own launch records — never from request fields, env
    /// (`CADENCE_ALIAS`), upstream grouping or an operator default.
    /// Memory resolves through here; reviews and reports are meant to.
    ///
    /// Every agent endpoint the daemon records is an identity node:
    ///
    /// - a pty endpoint's pane process (the agent row's pid), proven by
    ///   the adapter's native ownership check ([`Self::verify_pane_agent`]);
    /// - a managed endpoint's enrolled provider root (CAD-230: pid +
    ///   starttime + uid, bound to the owner generation), proven by the
    ///   strict enrollment verifier ([`Self::verify_enrolled_agent`]) —
    ///   so headless claude/codex authenticate exactly like panes.
    ///
    /// Exactly one node on the peer's ancestry is the agent. None is
    /// [`Caller::NoAgentIdentity`] — which never takes an agent's
    /// identity and is NOT operator proof (see that variant). Two or
    /// more (or one pid that is both a pane and an enrolled root) is
    /// ambiguous and refused, as is any node whose proof fails: fail
    /// closed, never fall through to another node.
    pub(super) fn caller_identity(&self, peer_pid: u32) -> Result<Caller> {
        // CAD-482: an asserted agent resolves straight from the
        // registry row — an unregistered name refuses, so the seam
        // asserts identities but never invents them. Liveness is the
        // fixture's business; the row need not prove a live endpoint.
        if let Some(asserted) = crate::test_seam::asserted() {
            return match asserted {
                crate::test_seam::Asserted::Agent(alias) => {
                    let agent = self.store.agent(&alias)?;
                    Ok(Caller::Agent(Box::new(VerifiedAgent {
                        generation: agent.generation.clone().unwrap_or_default(),
                        process_start: agent
                            .pid_start
                            .and_then(|s| u64::try_from(s).ok())
                            .unwrap_or(0),
                        agent,
                    })))
                }
                _ => Ok(Caller::NoAgentIdentity),
            };
        }
        // Drifted or closed owners lose their enrollment before it can
        // vouch for anyone.
        self.revalidate_enrollments()?;
        // /proc is still needed here, and only here: the peer's
        // ancestry is how a tool subprocess reaches its endpoint's
        // process. Linux-only; the macOS port is CAD-315.
        let chain = adapter::pty::caller_chain(peer_pid).ok_or_else(|| {
            Error::rejected(format!(
                "Caller pid {peer_pid}: /proc ancestry unreadable — caller \
                 identity underivable"
            ))
        })?;
        // A pane node is a row whose recorded start time still matches
        // (CAD-385): a reused pid is no node, an unproven one refuses.
        let recorded = crate::peer::AgentPids::classify(self.store.pty_pane_pids()?);
        recorded
            .refuse_unproven_on(&chain)
            .map_err(|why| Error::rejected(format!("Caller pid {peer_pid}: {why}")))?;
        let panes = recorded.live();
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let roots = slots.enrolled_roots_on(&chain);
        let mut nodes: Vec<String> = chain
            .iter()
            .filter_map(|pid| panes.get(pid).map(|alias| format!("pane '{alias}'")))
            .collect();
        for &r in &roots {
            if panes.contains_key(&chain[r]) {
                return Err(Error::rejected(format!(
                    "Caller pid {peer_pid}: pid {} is both a registered pane and an \
                     enrolled endpoint — caller identity ambiguous",
                    chain[r]
                )));
            }
            nodes.push(format!("enrolled root pid {}", chain[r]));
        }
        match nodes.len() {
            0 => return Ok(Caller::NoAgentIdentity),
            1 => {}
            n => {
                return Err(Error::rejected(format!(
                    "Caller pid {peer_pid} descends from {n} agent endpoints ({}) — \
                     caller identity ambiguous",
                    nodes.join(", ")
                )))
            }
        }
        if let Some(&r) = roots.first() {
            let strict = slots.strict_caller(peer_pid, chain[r])?;
            let enrollment = slots
                .endpoint_enrollment(&strict.enrollment_id)
                .cloned()
                .ok_or_else(|| {
                    Error::rejected(format!(
                        "Caller pid {peer_pid}: enrollment {} of '{}' is revoked (or a \
                         build runner's) — it vouches for no agent",
                        strict.enrollment_id, strict.lane
                    ))
                })?;
            drop(slots);
            return self
                .verify_enrolled_agent(enrollment)
                .map(|v| Caller::Agent(Box::new(v)));
        }
        drop(slots);
        let alias = adapter::pty::nearest_pane(&chain, &panes)
            .cloned()
            .expect("one pane node");
        self.verify_pane_agent(&alias)
            .map(|v| Caller::Agent(Box::new(v)))
    }

    /// A pty endpoint named by its pane: the row must be a live,
    /// generation-stamped endpoint and the adapter must still own that
    /// exact native session and pane process (unchanged CAD-191 proof).
    fn verify_pane_agent(&self, alias: &str) -> Result<VerifiedAgent> {
        let agent = self.store.agent(alias)?;
        require_live_endpoint(&agent)?;
        let generation = agent
            .generation
            .clone()
            .filter(|g| !g.is_empty() && agent.endpoint.is_some())
            .ok_or_else(|| {
                Error::rejected(format!("pty endpoint '{alias}' has no live generation"))
            })?;
        let pid = agent
            .pid
            .and_then(|p| u32::try_from(p).ok())
            .ok_or_else(|| Error::rejected("native endpoint pid disappeared"))?;
        // /proc: the pane's process start is read live and must be the
        // one the row recorded with its pid (CAD-385).
        let process_start = process_start_identity(pid)?;
        if agent.pid_start.and_then(|s| u64::try_from(s).ok()) != Some(process_start) {
            return Err(Error::rejected(format!(
                "pty endpoint '{alias}': pid {pid} is not the process the row \
                 recorded (no or a different process start time)"
            )));
        }
        let adapter = self.adapter_for(alias)?;
        adapter.verify_owned_endpoint(pid, &generation, agent.session_id.as_deref())?;
        if process_start != process_start_identity(pid)? {
            return Err(Error::rejected(
                "native endpoint process changed while resolving caller identity",
            ));
        }
        Ok(VerifiedAgent {
            agent,
            generation,
            process_start,
        })
    }

    /// A managed endpoint named by its active enrollment: the owner
    /// row, read now, must still be that endpoint — same owner
    /// generation (registration + endpoint generation + pid), same
    /// provider pid — and live. The process identity is the
    /// enrollment's own record (already re-verified hop by hop by
    /// [`crate::slots::Slots::strict_caller`]), so no further /proc
    /// read is needed.
    fn verify_enrolled_agent(&self, e: crate::slots::Enrollment) -> Result<VerifiedAgent> {
        let agent = self.store.agent_opt(&e.owner_actor)?.ok_or_else(|| {
            Error::rejected(format!(
                "enrollment {} names '{}', which is no longer registered",
                e.id, e.owner_actor
            ))
        })?;
        if owner_generation(&agent).as_deref() != Some(e.owner_generation.as_str())
            || agent.pid != Some(i64::from(e.root.pid))
            || agent.pid_start.and_then(|s| u64::try_from(s).ok()) != Some(e.root.starttime)
        {
            return Err(Error::rejected(format!(
                "enrollment {} of '{}' no longer matches its endpoint (owner \
                 generation, provider pid or its recorded start time changed)",
                e.id, e.owner_actor
            )));
        }
        require_live_endpoint(&agent)?;
        Ok(VerifiedAgent {
            agent,
            generation: e.owner_generation,
            process_start: e.root.starttime,
        })
    }

    /// Operator authority on POSITIVE proof only (CAD-276) — for
    /// `slot_reconcile` and the approval-evidence verbs (CAD-217), named
    /// by `verb` in refusals: deriving no slot identity is not enough —
    /// a detached child of a pane or managed tool derives none. See
    /// [`crate::peer::operator_proof`] for the checks; anything
    /// unreadable or ambiguous refuses.
    pub(super) fn proven_operator(&self, verb: &str, peer_pid: u32) -> Result<()> {
        self.operator_evidence(peer_pid).map_err(|why| {
            Error::rejected(format!(
                "{verb} is an operator action — this connection is not \
                 provably the operator: {why}; run it from an attached operator \
                 shell outside every pane and managed endpoint"
            ))
        })
    }

    /// [`crate::peer::operator_proof`] against the live panes and
    /// enrollments — `Err` names the first check that failed.
    ///
    /// The pane deny list is every row that MAY still be its process
    /// ([`crate::peer::AgentPids::fenced`], CAD-385): a row whose pid
    /// was reused names another process and denies nothing — exactly
    /// as if unregistered — while a row with no recorded start keeps
    /// denying (fail closed).
    pub(super) fn operator_evidence(&self, peer_pid: u32) -> std::result::Result<(), String> {
        // CAD-482: under a seam scope the assertion alone answers —
        // `unproven` refuses even where the ambient caller is provably
        // the operator, so a pane run and a CI run decide identically.
        if let Some(asserted) = crate::test_seam::asserted() {
            return match asserted {
                crate::test_seam::Asserted::Operator => Ok(()),
                crate::test_seam::Asserted::Agent(alias) => Err(format!(
                    "caller pid {peer_pid} is test-seam agent '{alias}' — \
                     not provably the operator"
                )),
                crate::test_seam::Asserted::Unproven => Err(format!(
                    "caller pid {peer_pid} carries a test-seam 'unproven' assertion — \
                     not provably the operator"
                )),
            };
        }
        let panes = crate::peer::AgentPids::classify(self.store.pty_pane_pids().map_err(|e| {
            format!("the registered panes cannot be read to prove this connection is not one ({e})")
        })?)
        .fenced();
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        crate::peer::operator_proof(
            peer_pid,
            unsafe { libc::geteuid() },
            std::process::id(),
            &panes,
            |pid| slots.nearest_enrolled_root(&[pid]).is_some(),
        )
    }

    /// The caller of an agent-mutating verb (CAD-149, CAD-304 S3), from
    /// the connection alone — the slot derivation, then positive
    /// operator proof:
    ///
    /// - the NEAREST registered pane or enrolled managed endpoint on
    ///   the peer's `/proc` ancestry IS that agent
    ///   ([`Self::slot_identity`]) — the unforgeable signal; a failed
    ///   strict verification or an ambiguous node refuses;
    /// - deriving none, the peer is the operator only on
    ///   [`crate::peer::operator_proof`] (CAD-276): a `setsid` detach
    ///   of a pane that keeps its env or pty, a process the daemon
    ///   launched (an unenrolled managed worker's tools), an orphaned
    ///   session — all refused, never defaulted to `operator`.
    ///
    /// `CADENCE_ALIAS`, `--reviewer`-style claims and request fields
    /// never participate. Residual, as for `slot_reconcile`: a same-uid
    /// process that leaves every pane's ancestry without orphaning its
    /// session and scrubs its env and stdio passes operator proof —
    /// CAD-280 (operator by positive proof) is where that tightens.
    pub(super) fn agent_caller(&self, peer_pid: u32, verb: &str) -> Result<AgentCaller> {
        match self.connection_caller(peer_pid)? {
            caller_rule::Who::Operator => Ok(AgentCaller::Operator),
            caller_rule::Who::Agent(alias) => Ok(AgentCaller::Agent(alias)),
            caller_rule::Who::Unproven(why) => Err(Error::rejected(format!(
                "{verb} refused: this connection derives no agent identity and \
                 is not provably the operator: {why}. Run it from the calling \
                 agent's own pane, or from an attached operator shell outside \
                 every pane and managed endpoint (caller rule, CAD-149)"
            ))),
        }
    }

    /// The connection's caller for the one caller rule (CAD-149,
    /// CAD-384) — see [`Self::agent_caller`] for the derivation. A pane
    /// whose agent cannot be named, or no agent identity without
    /// operator proof, is [`caller_rule::Who::Unproven`]; an ancestry
    /// that cannot be verified (a failed strict check, an ambiguous
    /// node) refuses outright.
    pub(super) fn connection_caller(&self, peer_pid: u32) -> Result<caller_rule::Who> {
        use caller_rule::Who;
        // CAD-482: the frame-level assertion is the caller for this
        // dispatch — the runner's pane/CI environment cannot leak in.
        if let Some(asserted) = crate::test_seam::asserted() {
            return Ok(match asserted {
                crate::test_seam::Asserted::Operator => Who::Operator,
                crate::test_seam::Asserted::Agent(alias) => Who::Agent(alias),
                crate::test_seam::Asserted::Unproven => Who::Unproven(format!(
                    "caller pid {peer_pid} carries a test-seam 'unproven' assertion \
                     — it derives no identity"
                )),
            });
        }
        self.revalidate_enrollments()?;
        if let Some(who) = self.slot_identity(peer_pid)? {
            let lane = who.lane();
            if lane.is_empty() {
                return Ok(Who::Unproven(format!(
                    "caller pid {peer_pid} descends from a pane whose agent cannot \
                     be named — caller identity underivable"
                )));
            }
            return Ok(Who::Agent(lane.to_string()));
        }
        Ok(match self.operator_evidence(peer_pid) {
            Ok(()) => Who::Operator,
            Err(why) => Who::Unproven(why),
        })
    }

    /// CAD-384: in a sandbox daemon only, a caller tied to NONE of the
    /// sandbox's agents — the shape of `cadence sandbox down` run from a
    /// production agent's pane, whose `CADENCE_ALIAS` names no sandbox
    /// agent. It is unproven only because of production's pane env; in
    /// its own disposable sandbox it may stop agents and the daemon.
    /// Tied means any of: a pane or enrolled endpoint of this daemon on
    /// its ancestry, a descendant of this daemon (everything it
    /// launched), a pane's pty on its stdio, or a `CADENCE_ALIAS` on any
    /// hop that names an agent registered here. Anything unreadable is
    /// tied (fail closed): the ancestry, the pane facts, a hop's uid, and
    /// the environment of any hop running as this daemon's uid — agents
    /// run as that uid, and its own processes' environments are readable.
    /// A hop of ANOTHER uid (a root `sshd`, whose environment the kernel
    /// hides) cannot be an agent and is skipped, as `peer::operator_proof`
    /// skips it.
    pub(super) fn sandbox_outsider(&self, peer_pid: u32) -> bool {
        if !crate::rollout::sandbox_exempt(&self.state_dir) {
            return false;
        }
        let Some(chain) = adapter::pty::caller_chain(peer_pid) else {
            return false;
        };
        let me = std::process::id();
        if chain.iter().skip(1).any(|&hop| hop == me) {
            return false;
        }
        // Pane rows deny through the fenced view (CAD-385): a row whose
        // pid was reused ties nothing, one with no recorded start still
        // ties.
        let Ok(rows) = self.store.pty_pane_pids() else {
            return false;
        };
        let panes = crate::peer::AgentPids::classify(rows).fenced();
        {
            let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            if chain.iter().any(|hop| {
                panes.contains_key(hop) || slots.nearest_enrolled_root(&[*hop]).is_some()
            }) {
                return false;
            }
        }
        let ties = PeerTies::probe(peer_pid);
        if !ties.walked()
            || !ties
                .agents(panes.iter().map(|(pid, alias)| (alias.as_str(), *pid)))
                .is_empty()
        {
            return false;
        }
        let uid = unsafe { libc::geteuid() };
        chain.iter().all(|&hop| {
            match crate::peer::proc_uids(hop) {
                Ok((real, effective)) if real != uid && effective != uid => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
            match proc_env_alias(hop) {
                Err(()) => false,
                Ok(None) => true,
                Ok(Some(alias)) => matches!(self.store.agent_opt(&alias), Ok(None)),
            }
        })
    }

    /// CAD-384: apply `method`'s caller rule ([`caller_rule::RULES`])
    /// before it runs. `Ok(Some(params))` is the request with its
    /// attribution field stamped to the caller; a refusal happens
    /// before any write. Only reads are made here: the connection's
    /// `/proc` ancestry, the target row, the rollout lease.
    pub(super) fn caller_gate(
        &self,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Option<Value>> {
        use caller_rule::{Facts, Rule, Target, Who};
        let Some(rule) = caller_rule::rule_of(method) else {
            return Ok(None);
        };
        if !rule.checks_connection() {
            return Ok(None);
        }
        let who = self.connection_caller(peer_pid)?;
        let mut facts = Facts::default();
        match (rule, &who) {
            (Rule::OnAgent(target, _), Who::Agent(_)) => {
                let alias = match target {
                    Target::Alias => self.resolve_alias(required_str(params, "alias")?)?,
                    Target::Message => {
                        let id = required_str(params, "message")?;
                        self.store
                            .message(id)?
                            .ok_or_else(|| Error::rejected(format!("Unknown message '{id}'")))?
                            .alias
                    }
                };
                let agent = self.store.agent(&alias)?;
                facts.target = Some((alias, self.effective_pm(&agent)?));
            }
            (Rule::Shutdown, Who::Agent(_)) => {
                facts.lease_holder = crate::rollout::granted_lease_holder(&self.state_dir)?;
            }
            (Rule::Shutdown | Rule::OnAgent(..), Who::Unproven(_)) => {
                facts.sandbox_outsider = self.sandbox_outsider(peer_pid);
            }
            _ => {}
        }
        let stamp =
            caller_rule::admit(method, rule, &who, params, &facts).map_err(Error::rejected)?;
        Ok(stamp.map(|(field, value)| {
            let mut stamped = params.clone();
            if !stamped.is_object() {
                stamped = json!({});
            }
            stamped[field] = value;
            stamped
        }))
    }

    /// Authorize one agent-mutating request against `target` (CAD-149,
    /// CAD-304 S3): identity-shaped request fields are refused, the
    /// caller is derived ([`Self::agent_caller`]) and the one policy
    /// ([`crate::peer::may_mutate_agent`]) decides with the target's own
    /// PM (`params.upstream`). Returns the caller for the audit stamp.
    pub(super) fn authorize_agent_mutation(
        &self,
        params: &Value,
        peer_pid: u32,
        verb: &str,
        target: &Agent,
        mutation: AgentMutation,
    ) -> Result<AgentCaller> {
        reject_identity_fields(params, verb)?;
        let caller = self.agent_caller(peer_pid, verb)?;
        let pm = self.effective_pm(target)?;
        crate::peer::may_mutate_agent(&caller, &target.alias, pm.as_deref(), mutation, verb)
            .map_err(Error::rejected)?;
        Ok(caller)
    }

    /// Registration is a mutation of the new agent by its caller
    /// (CAD-149 review F3): an agent the daemon can attribute the
    /// connection to may register only its OWN member — the new row's
    /// `params.upstream` must name the caller — and only when the
    /// caller is a group root (no upstream of its own; groups are one
    /// level deep). So a worker can neither mint an agent that carries
    /// trust-bearing params it may not set on itself, nor make itself
    /// (or a peer) a PM, nor create a root only the operator controls;
    /// a PM's `join` into its own group works unchanged, with any
    /// params. An alias that already exists is left to the store's
    /// duplicate refusal — registration changes nothing then, and the
    /// launch verbs' reopen path keys off that error.
    ///
    /// A connection attributed to no agent is not required to prove
    /// it is the operator here (unlike `agent set`/`remove`): every
    /// operator script, launch verb and test harness registers that
    /// way, including from a shell whose ancestry carries an agent's
    /// environment. The price is the F1 residual in a wider form — a
    /// detached worker process registers unchecked — which CAD-280
    /// (operator by positive proof) closes for both.
    pub(super) fn authorize_register(
        &self,
        alias: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<()> {
        if self.store.agent_opt(alias)?.is_some() {
            return Ok(());
        }
        self.revalidate_enrollments()?;
        let Some(who) = self.slot_identity(peer_pid)? else {
            // No agent on the connection is not the operator: a worker's
            // detached (setsid + fork) child derives no identity either,
            // and would otherwise mint a root agent outside its group —
            // one the review loop would take as independent (CAD-431).
            // Unattributed registration needs positive operator proof.
            return self.proven_operator("agent register", peer_pid);
        };
        let caller = who.lane();
        let upstream = params
            .get("upstream")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty());
        let caller_pm = match self.store.agent_opt(caller)? {
            Some(row) => agent_upstream(&row).map(str::to_string),
            None => {
                return Err(Error::rejected(format!(
                    "agent register refused: caller pid {peer_pid} descends from \
                     pane '{caller}', which names no registered agent"
                )))
            }
        };
        if upstream == Some(caller) && caller_pm.is_none() {
            return Ok(());
        }
        let why = match (&caller_pm, upstream) {
            (Some(pm), _) => format!("'{caller}' is a worker in '{pm}''s group"),
            (None, Some(u)) => format!("the new agent would answer to '{u}', not to '{caller}'"),
            (None, None) => "the new agent would be a group root".to_string(),
        };
        Err(Error::rejected(format!(
            "agent register refused: agent '{caller}' may register only its own \
             members (params.upstream = '{caller}') and only as a group root — \
             {why}. Ask your PM or the operator to register '{alias}' \
             (caller rule, CAD-149)"
        )))
    }

    /// The target's PM for the caller rule: its `params.upstream`, but
    /// only while the agent registered under that alias predates the
    /// target (CAD-149 review F2). PM authority is bound to the PM's
    /// registration, never to the name: a PM that was removed and whose
    /// alias was registered again — by anyone — gains nothing over the
    /// members the old PM left behind; they answer to the operator
    /// until re-homed. `join` resolves the PM before registering the
    /// member, so a real PM is always older. Timestamps are the store's
    /// sub-second wall clock; a backwards clock step between a PM's
    /// removal and its alias's re-registration is the residual.
    pub(super) fn effective_pm(&self, target: &Agent) -> Result<Option<String>> {
        let Some(pm) = agent_upstream(target) else {
            return Ok(None);
        };
        Ok(self
            .store
            .agent_opt(pm)?
            .filter(|row| row.created <= target.created)
            .map(|row| row.alias))
    }

    /// The `agent gc` candidates split by the caller rule: `(permitted,
    /// not_permitted)` — the same decision `agent remove` makes.
    pub(super) fn gc_partition(
        &self,
        caller: &AgentCaller,
        older_than: Option<f64>,
    ) -> Result<(Vec<Agent>, Vec<String>)> {
        let mut permitted = Vec::new();
        let mut not_permitted = Vec::new();
        for agent in self.store.gc_candidates(older_than)? {
            let pm = self.effective_pm(&agent)?;
            let allowed = crate::peer::may_mutate_agent(
                caller,
                &agent.alias,
                pm.as_deref(),
                AgentMutation::Controlled,
                "agent gc",
            )
            .is_ok();
            if allowed {
                permitted.push(agent);
            } else {
                not_permitted.push(agent.alias);
            }
        }
        Ok((permitted, not_permitted))
    }

    /// Operator authority for the approval-evidence verbs (CAD-217) and
    /// `model_defaults_set` (CAD-337): exactly the connection-bound rule
    /// `slot_reconcile` applies. A
    /// caller whose `SO_PEERCRED` ancestry reaches a registered pane or
    /// an enrolled managed endpoint is an agent and is refused, and so
    /// is one that is not provably the operator
    /// ([`Self::proven_operator`]). Identity-shaped request fields are
    /// refused rather than read — a worker's output or message can
    /// never name who recorded the evidence or made the change. Residual (CAD-276's, see
    /// docs/AUDIT.md; CAD-280 replaces the rule): a same-uid process
    /// that leaves every agent's ancestry without orphaning its session
    /// and scrubs its env and stdio still passes.
    pub(super) fn operator_connection(
        &self,
        verb: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<()> {
        reject_operator_fields(verb, params)?;
        if let Some(who) = self.slot_identity(peer_pid)? {
            return Err(Error::rejected(format!(
                "{verb} is an operator action — this connection is agent \
                 '{}'; run it outside every pane and managed endpoint",
                who.lane()
            )));
        }
        self.proven_operator(verb, peer_pid)
    }

    /// [`Self::operator_connection`] for a verb whose `alias` param
    /// names the TARGET agent (`agent unfence`), not the caller: the
    /// same connection gate and identity-field refusal, with `alias`
    /// left to the verb (CAD-374).
    pub(super) fn operator_connection_on_agent(
        &self,
        verb: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<()> {
        let mut fields = params.clone();
        if let Some(object) = fields.as_object_mut() {
            object.remove("alias");
        }
        reject_identity_fields(&fields, verb)?;
        self.operator_connection(verb, &fields, peer_pid)
    }
}

/// Who is on the other end of a connection — see
/// [`Shared::caller_identity`] (CAD-381).
pub(super) enum Caller {
    /// No agent endpoint on the caller's ancestry: it never resolves
    /// to, or borrows, an agent's identity. This is NOT operator proof —
    /// an agent's own process escapes every agent tree by `setsid` +
    /// double fork, `systemd-run` or a new tmux session. Operator
    /// authority needs its own positive proof
    /// ([`Shared::proven_operator`], CAD-276; web operator auth CAD-313).
    NoAgentIdentity,
    /// Exactly one live agent endpoint, proven from the daemon's record.
    Agent(Box<VerifiedAgent>),
}

/// An agent endpoint the daemon verified for this connection.
pub(super) struct VerifiedAgent {
    pub(super) agent: Agent,
    /// The endpoint's proof generation: the pty adapter generation, or
    /// a managed endpoint's enrolled owner generation.
    pub(super) generation: String,
    /// The endpoint process's start time (`/proc` starttime).
    pub(super) process_start: u64,
}

/// A verified identity needs an endpoint that is up right now.
fn require_live_endpoint(agent: &Agent) -> Result<()> {
    if matches!(
        agent.state.as_str(),
        "idle" | "busy" | "running" | "waiting_input"
    ) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "agent '{}' is {} — only a live endpoint has a caller identity",
            agent.alias, agent.state
        )))
    }
}

/// Who a slot call runs as — see [`Shared::slot_identity`].
pub(super) enum SlotWho {
    /// Legacy binding: the nearest registered pty pane (CAD-113).
    Pane { lane: String, chain: Vec<u32> },
    /// Strict binding: a verified caller of a managed endpoint's
    /// enrollment (CAD-230).
    Strict(crate::slots::StrictCaller),
}

impl SlotWho {
    pub(super) fn lane(&self) -> &str {
        match self {
            SlotWho::Pane { lane, .. } => lane,
            SlotWho::Strict(c) => &c.lane,
        }
    }

    /// Every pid the caller may bind a hold to: the whole ancestry for
    /// a pane caller, the verified peer-to-root segment for a strict one.
    pub(super) fn chain(&self) -> &[u32] {
        match self {
            SlotWho::Pane { chain, .. } => chain,
            SlotWho::Strict(c) => &c.segment,
        }
    }
}

/// An enrollment's owner generation, read from the owner row: the
/// registration instant, the adapter's endpoint generation and the
/// recorded provider pid. A re-registration, a reopen or a closed
/// endpoint (pid cleared) all change or erase it — `None` means the
/// owner has no live endpoint to enroll.
fn owner_generation(agent: &Agent) -> Option<String> {
    let pid = agent.pid?;
    Some(format!(
        "{:016x}:{}:{pid}",
        agent.created.to_bits(),
        agent.generation.as_deref().unwrap_or("-")
    ))
}
