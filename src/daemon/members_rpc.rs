//! CAD-1380/AOS-216: `member_*` daemon RPC handlers — the member plane the
//! board's `/api/assist/<verb>` route relays to.
//!
//! A member never reaches the daemon's Unix socket and never carries an
//! agent/operator caller identity. The member proof is the **signed
//! assistant envelope itself** (`member_envelope`): the `/api/assist/*`
//! route verifies it, consumes its single-use `jti` at the HTTP boundary
//! (the ONE defined replay-consumption point), and forwards the raw signed
//! bytes to the daemon; the daemon then **re-verifies the Ed25519
//! signature and every claim pin against its own `board-identity.json` +
//! the platform JWKS** — daemon-verifiable authority, so the board relay
//! (or any caller) can never widen member authority by asserting a
//! `member_as`-style field it merely claims. The daemon does NOT re-consume
//! the jti (the route is the single consumption point); it re-checks the
//! cryptography only.
//!
//! `member_envelope` is honored only on a daemon-proven operator/board
//! connection — the channel the board's relay runs on. An agent
//! connection, an unproven connection, or any caller presenting a
//! `member_envelope` is refused at `proven_operator` before the envelope
//! is even read. After cryptographic re-verify the daemon re-binds the
//! envelope's `(org, sub, assistant)` to the stored `assistants` row it
//! owns (created by daemon-side provisioning, never by request), refusing
//! a triple naming no binding, a different org, or a non-`active` state.
//!
//! Every member effect re-checks the stored binding's `state` — `active`
//! admits, `revoked`/`archived` refuse. The live platform-membership
//! check happens on the board side at mint + at every `/call`
//! (`resolveWikiActor` re-reads `workspace_members`); a removed member
//! cannot mint or call. The deferred-effect commit-time revalidation
//! (kickoff §4) the platform's `POST /__platform/assistant/revalidate`
//! supplies is NOT yet invoked from a daemon commit path — see the
//! "Open gaps" note in the interface doc; no revocation acceptance is
//! claimed for a queued/resumed effect beyond the stored-binding refuse.

use std::io::Read as _;

use super::*;

use crate::board_identity;

/// Refuse any request field outside `allowed` — the member surface admits
/// only `member_envelope` plus the verb's own arguments; an unlisted or
/// identity field is never silently dropped.
fn reject_unknown_fields(params: &Value, allowed: &[&str], verb: &str) -> Result<()> {
    let object = params
        .as_object()
        .ok_or_else(|| Error::rejected(format!("{verb} takes an object")))?;
    if let Some(field) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::rejected(format!(
            "{verb} takes {} only; field '{field}' is not accepted",
            allowed.join(", ")
        )));
    }
    Ok(())
}

/// A daemon-proven member caller — bound to a stored assistant row AND
/// re-verified against the signed envelope the board relayed.
pub(crate) struct MemberCaller {
    pub org: String,
    pub sub: String,
    pub assistant_id: String,
    /// The daemon-owned agent alias the assistant runs as.
    pub alias: String,
    pub conversation_id: Option<String>,
    /// Binding state at admission (re-checked every effect; `active` only).
    #[allow(dead_code)]
    pub state: String,
    /// The envelope's issuing credential — the member's recorded authority
    /// provenance a deferred commit re-proves.
    pub credential_id: String,
}

/// The operation + scope + (optional) job + op provenance a
/// `member_authority_live` re-prove carries — the strict revalidate
/// body's bound fields. `op_credential_id` is the queued effect's own
/// CAD-1380 R4: pending assist intents drained per tick (oldest first).
pub(super) const ASSIST_NOTICE_BATCH: usize = 20;

/// A bounded, single-line diagnostic for an unresolved intent (no secrets:
/// the reason is a refusal message, never a credential).
fn bounded_reason(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(240).collect()
}

/// A required string field of a stored intent; empty or absent refuses.
fn intent_str(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// recorded `assist_requests.credential_id` — required for
/// `job_dispatch`, absent for every assistant-resource verb.
pub(super) struct MemberReprove<'a> {
    pub operation: &'a str,
    pub scope: &'a str,
    pub job_id: Option<&'a str>,
    /// The queued effect's own recorded provenance — required for
    /// `job_dispatch`, absent for every other op. The platform re-proves
    /// THIS credential live, never the binding's latest pin.
    pub op_credential_id: Option<&'a str>,
}

/// Daemon-side JWKS cache for the assist family — independent of the HTTP
/// route's, so the daemon's re-verify does not depend on the route's cache.
static MEMBER_JWKS: Mutex<crate::board_identity::JwksCache> =
    Mutex::new(crate::board_identity::JwksCache::new());

impl Shared {
    /// CAD-1380: a member-bound personal assistant (`assist-*` alias)
    /// reaches only the member-assistant verb surface — its own
    /// conversation/thread reads, its own message report, the job
    /// records bound to it, and the assist plumbing. Every generic
    /// worker/operator verb — other agents, issues, wiki, dispatch,
    /// register, operator effects — is refused here, before the method
    /// runs, by the derived caller's alias class. This is the
    /// member-boundary complement to `master_policy`: it scopes the
    /// member's own runtime (a worker agent), never widens it.
    ///
    /// The check reads only the derived caller identity — never a
    /// request field. A connection that derives no agent identity
    /// (operator, unproven) is untouched: it takes its own rule.
    pub(super) fn member_assist_policy(
        &self,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<()> {
        // `member_*` verbs themselves are gated by `member_caller` (the
        // proven relay + signed envelope), not by this agent-class gate —
        // an assist alias never calls them itself.
        if method.starts_with("member_") {
            return Ok(());
        }
        // CAD-1380 target-side privacy: a member-owned read is refused for an
        // ordinary or unproven caller BEFORE the handler runs, keyed on the
        // canonical target from the real store (never a raw prefix or request
        // claim). Operator and bound-owner access stay on their own rules.
        self.member_target_read_guard(method, params, peer_pid)?;
        let alias = match self.caller_identity(peer_pid) {
            Ok(Caller::Agent(v)) => v.agent.alias.clone(),
            _ => return Ok(()),
        };
        // The member boundary is SOURCE-VERIFIED and fail-CLOSED: the
        // caller is a member assistant iff the `assistants` table holds
        // a binding for this alias — the immutable (org, sub) record,
        // never a request field or a name prefix. A revoked/archived
        // binding KEEPS the alias inside the member boundary (it is
        // still a member assistant, just a dead one — the per-resource
        // checks below still refuse); it does NOT drop to generic
        // worker rules. A store read failure here is an error, not an
        // "unbound" answer — the member boundary never falls open on an
        // unreadable binding table.
        let binding = match self.store.assistant_for_alias(&alias) {
            Ok(Some(b)) => b,
            Ok(None) => return Ok(()), // not a member assistant — ordinary worker/operator rule
            Err(e) => {
                return Err(Error::rejected(format!(
                    "member assist refused: the assistant binding for '{alias}' could not be \
                     read ({e}) — the member boundary fails closed"
                )));
            }
        };
        // CAD-1380 R1: a revoked/archived binding never falls back to
        // generic rules; it is refused for every method, before the surface.
        if !binding.is_active() {
            return Err(Error::rejected(format!(
                "assist agent '{alias}' may not {method}: its assistant binding is {} — \
                 a revoked or archived member has no surface (CAD-1380)",
                binding.state
            )));
        }
        // The member-assistant verb surface — a bounded whitelist, each
        // entry carrying the per-resource check its handler needs. A
        // method absent here is refused before it runs; a method
        // present but naming a resource outside the member's own
        // surface is refused by the per-verb arm below.
        match method {
            // Own alias only: the agent reads/steers/interrupts itself.
            "agent_show" | "agent_events" | "agent_send" | "agent_ask" => {
                let target = params
                    .get("alias")
                    .or_else(|| params.get("to"))
                    .or_else(|| params.get("agent"))
                    .and_then(Value::as_str);
                // A missing target means the handler resolves to the
                // caller's own alias; a present one must equal it.
                if let Some(t) = target {
                    if t != alias {
                        return Err(Error::rejected(format!(
                            "assist agent '{alias}' may not {method} '{t}' — only its own \
                             principal surface (CAD-1380)"
                        )));
                    }
                }
            }
            // Own conversation thread only.
            "thread_read" => {
                let target = params
                    .get("alias")
                    .and_then(Value::as_str)
                    .unwrap_or(alias.as_str());
                if target != alias {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not thread_read '{target}' — only its own \
                         conversation (CAD-1380)"
                    )));
                }
            }
            // Own messages only. `message_report` resolves its target through
            // the existing caller-bound resolver (its own held turn when no id
            // is named, or an explicit id/prefix limited to this alias), then
            // asserts the resolved message belongs to this alias. Downstream
            // token/generation/state gates still run in the handler.
            "message_report" => {
                // Every form resolves through the caller-bound resolver first
                // (exact id, unique own prefix, or the held turn), so ownership
                // is checked on the CANONICAL id, never a raw prefix.
                let id = match params.get("message").and_then(Value::as_str) {
                    Some(named) => self.resolve_message_id_for(peer_pid, named)?,
                    None => self.report_target(params, peer_pid)?.0,
                };
                let own = self
                    .store
                    .message(&id)
                    .ok()
                    .flatten()
                    .map(|m| m.alias == alias)
                    .unwrap_or(false);
                if !own {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not {method} '{id}' — the message is not \
                         its own (CAD-1380)"
                    )));
                }
            }
            // Own recipient only. `to` is required; there is no implicit self.
            "message_list" => {
                let target = params.get("to").and_then(Value::as_str).unwrap_or("");
                if target != alias {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not {method} '{target}' — only its own \
                         mail (CAD-1380)"
                    )));
                }
            }
            // `message_read` names an id; bound to this alias's message row.
            "message_read" => {
                let id = params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let own = self
                    .store
                    .message(id)
                    .ok()
                    .flatten()
                    .map(|m| m.alias == alias)
                    .unwrap_or(false);
                if !own {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not {method} '{id}' — the message is not \
                         its own (CAD-1380)"
                    )));
                }
            }
            // Own bound jobs only: `job_show`/`job_events` name a job
            // this member's assistant owns or subscribes.
            "job_show" | "job_events" => {
                let job = params
                    .get("job")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let owns = self
                    .store
                    .assist_request_owns(&binding.org, &binding.sub, job)
                    .unwrap_or(false);
                let subscribes = self
                    .store
                    .job_is_subscriber(job, &binding.org, &binding.sub)
                    .unwrap_or(false);
                if !owns && !subscribes {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not {method} '{job}' — the job is not \
                         bound to its principal (CAD-1380)"
                    )));
                }
            }
            // A task read is scoped through its job's ownership.
            "task_show" => {
                let task_id = params
                    .get("task")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let job_id = self
                    .store
                    .task(task_id)
                    .ok()
                    .map(|t| t.job_id)
                    .unwrap_or_default();
                let owns = self
                    .store
                    .assist_request_owns(&binding.org, &binding.sub, &job_id)
                    .unwrap_or(false);
                let subscribes = self
                    .store
                    .job_is_subscriber(&job_id, &binding.org, &binding.sub)
                    .unwrap_or(false);
                if !owns && !subscribes {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not task_show '{task_id}' — the task is \
                         not bound to its principal (CAD-1380)"
                    )));
                }
            }
            // Interrupt: own live turn only — the alias it names must be
            // itself and a turn must actually be running.
            "interrupt" => {
                let target = params
                    .get("alias")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let running = self.store.running_message(&alias).ok().flatten().is_some();
                if target != alias || !running {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not interrupt '{target}' — only its own \
                         live turn (CAD-1380)"
                    )));
                }
            }
            // Every other method — including broad reads like `health`,
            // `daemon_info`, `agent_list`, `job_list`, issues, wiki,
            // dispatch, register, plans and every operator effect — is
            // refused for a member assistant. Its surface is its own
            // conversation, its own mail and the jobs bound to it.
            _ => {
                return Err(Error::rejected(format!(
                    "assist agent '{alias}' may not call '{method}' — its surface is \
                     its own conversation and the jobs bound to it; fleet/issue/wiki/\
                     dispatch/operator effects are refused (member boundary, CAD-1380)"
                )));
            }
        }
        // CAD-1380 R1: every admitted member surface re-proves the member's
        // CURRENT platform authority via the live pull (enrolled hosted-board
        // custody). The provenance is the binding's recorded issuing
        // credential, because this connection carries no member envelope.
        let (operation, scope) = match method {
            "agent_show" | "agent_events" => ("assist_state", "assist.read"),
            "thread_read" | "message_read" | "message_report" | "message_list" => {
                ("assist_history", "assist.read")
            }
            "job_show" | "task_show" => ("job_evidence", "assist.jobs"),
            "job_events" => ("job_events", "assist.jobs"),
            _ => ("assist_send", "assist.write"),
        };
        let member = MemberCaller {
            org: binding.org.clone(),
            sub: binding.sub.clone(),
            assistant_id: binding.assistant_id.clone(),
            alias: binding.alias.clone(),
            conversation_id: binding.conversation_id.clone(),
            state: binding.state.clone(),
            credential_id: String::new(),
        };
        self.member_authority_live(
            "",
            &member,
            MemberReprove {
                operation,
                scope,
                job_id: None,
                op_credential_id: None,
            },
            method,
        )?;
        Ok(())
    }

    /// CAD-1380 target-side guard for the reachable member-owned READS. A
    /// member-private target (an assistant alias, its thread/events, or a job
    /// or task bound to a member) is refused for every caller except the
    /// operator and the bound owner. Jobs and tasks additionally admit their
    /// canonical PM and assigned task author through existing explicit
    /// assignment. Ordinary nonmember targets are untouched.
    fn member_target_read_guard(&self, method: &str, params: &Value, peer_pid: u32) -> Result<()> {
        let (alias_arg, job_arg, task_arg) = match method {
            "thread_read" | "agent_show" | "agent_events" | "agent_wait" => {
                (params.get("alias").and_then(Value::as_str), None, None)
            }
            "message_list" => (params.get("to").and_then(Value::as_str), None, None),
            "job_show" | "job_events" => (None, params.get("job").and_then(Value::as_str), None),
            "task_show" => (None, None, params.get("task").and_then(Value::as_str)),
            _ => return Ok(()),
        };
        // Resolve the CANONICAL target before any ownership decision, so an
        // alias prefix, conversation or task form cannot dodge the check.
        if let Some(raw) = alias_arg {
            let Ok(canonical) = self.resolve_alias(raw) else {
                return Ok(());
            };
            let Some(binding) = self.store.assistant_for_alias(&canonical)? else {
                return Ok(());
            };
            return self.member_owner_or_operator(
                method,
                peer_pid,
                Some(&binding),
                &binding.org,
                None,
            );
        }
        // A job or task read names its job through the real store. An
        // unknown id is not member-private (the handler errors for it); any
        // real store error propagates (fail closed), never a silent Ok.
        let (job, task) = match (job_arg, task_arg) {
            (Some(job), _) => (job.to_string(), None),
            (None, Some(task)) => match self.store.task(task) {
                Ok(t) => (t.job_id, Some(task.to_string())),
                Err(crate::Error::Rejected(_)) => return Ok(()),
                Err(e) => return Err(e),
            },
            (None, None) => return Ok(()),
        };
        let owner = match self.store.assist_request_for_job(&job) {
            Ok(Some(owner)) => owner,
            Ok(None) => return Ok(()),
            Err(e) => return Err(e),
        };
        // The job has a member-owned ledger row, so it is member-private. A
        // missing owner binding can never drop it to ordinary rules.
        let binding = self.store.assistant_binding(&owner.org, &owner.sub)?;
        self.member_owner_or_operator(
            method,
            peer_pid,
            binding.as_ref(),
            &owner.org,
            Some((&job, task.as_deref())),
        )
    }

    /// The shared decision for a member-owned target: the operator and the
    /// bound assistant that owns it pass; an explicitly assigned job PM or
    /// task author passes for a job (never for member-private chat); anyone
    /// else (ordinary worker, unproven) is refused.
    /// `binding` is the owner's live binding, or `None` when the ledger row
    /// names an owner with no binding (never dropped to ordinary rules).
    /// `job` is `Some((job_id, requested_task))` for a job or task read.
    fn member_owner_or_operator(
        &self,
        method: &str,
        peer_pid: u32,
        binding: Option<&crate::store::AssistantBinding>,
        org: &str,
        job: Option<(&str, Option<&str>)>,
    ) -> Result<()> {
        let refuse = |who: &str| {
            Err(Error::rejected(format!(
                "{method} refused: '{who}' cannot read a member-private target \
                 (CAD-1380 target-side privacy)"
            )))
        };
        match self.connection_caller(peer_pid)? {
            caller_rule::Who::Operator => Ok(()),
            caller_rule::Who::Agent(caller) => {
                if binding.is_some_and(|b| caller == b.alias) {
                    return Ok(());
                }
                if let Some((job, task)) = job {
                    if self.caller_holds_job_authority(&caller, job, task)? {
                        return Ok(());
                    }
                    // A subscribing member's bound assistant may read a shared
                    // job's record and events. The caller must be its own active
                    // binding in the same org with a live subscriber row for this
                    // job, and the owner must still share it. Never private chat.
                    if self.caller_is_active_subscriber(&caller, job, org)? {
                        return Ok(());
                    }
                }
                refuse(&caller)
            }
            caller_rule::Who::Unproven(why) => Err(Error::rejected(format!(
                "{method} refused: an unproven caller cannot read a member-private target \
                 ({why}) (CAD-1380 target-side privacy)"
            ))),
        }
    }

    /// Is the caller an ACTIVE bound assistant in `org` whose principal is a
    /// live subscriber of `job`, while the owner still shares the job?
    fn caller_is_active_subscriber(&self, caller: &str, job: &str, org: &str) -> Result<bool> {
        let Some(me) = self.store.assistant_for_alias(caller)? else {
            return Ok(false);
        };
        Ok(me.is_active()
            && me.org == org
            && self.store.job_is_subscriber(job, org, &me.sub)?
            && self.store.assist_job_is_shared(job, org)?)
    }

    /// CAD-1380 list filter for `agent_list`. Returns `true` when the row must
    /// be withheld from this caller because it is a member-private assistant
    /// row the caller is not the owner of. The operator sees every row, a bound
    /// assistant sees its own row, and nonmember rows are never withheld.
    pub(super) fn member_agent_row_withheld(&self, alias: &str, peer_pid: u32) -> Result<bool> {
        let Some(binding) = self.store.assistant_for_alias(alias)? else {
            return Ok(false);
        };
        Ok(match self.connection_caller(peer_pid)? {
            caller_rule::Who::Operator => false,
            caller_rule::Who::Agent(caller) => caller != binding.alias,
            caller_rule::Who::Unproven(_) => true,
        })
    }

    /// CAD-1380 list filter for `job_list`. Returns `true` when the job must be
    /// withheld: it is bound to a member's assistant and the caller is neither
    /// the operator nor an authorized job actor (canonical PM or the assignee
    /// of a task of this job). Nonmember jobs are never withheld.
    pub(super) fn member_job_row_withheld(&self, job: &str, peer_pid: u32) -> Result<bool> {
        let Some(owner) = self.store.assist_request_for_job(job)? else {
            return Ok(false);
        };
        let binding = self.store.assistant_binding(&owner.org, &owner.sub)?;
        match self.connection_caller(peer_pid)? {
            caller_rule::Who::Operator => Ok(false),
            caller_rule::Who::Agent(caller) => {
                if binding.as_ref().is_some_and(|b| caller == b.alias) {
                    return Ok(false);
                }
                if self.caller_holds_job_authority(&caller, job, None)? {
                    return Ok(false);
                }
                Ok(!self.caller_is_active_subscriber(&caller, job, &owner.org)?)
            }
            caller_rule::Who::Unproven(_) => Ok(true),
        }
    }

    /// Job-level authority: the canonical job PM (the whole job context), or
    /// the assignee of the single task the caller asked for. Another task's
    /// assignee, and any generic worker, gets nothing here.
    fn caller_holds_job_authority(
        &self,
        caller: &str,
        job: &str,
        requested_task: Option<&str>,
    ) -> Result<bool> {
        if self.store.job(job)?.pm_alias == caller {
            return Ok(true);
        }
        match requested_task {
            Some(task) => Ok(self.store.task(task)?.assignee.as_deref() == Some(caller)),
            None => Ok(false),
        }
    }

    /// Re-verify the member's signed envelope at the daemon. The config is
    /// the daemon's own `board-identity.json` (issuer/audience/company the
    /// operator configured), never a request field. Returns the verified
    /// actor's `(org, sub, assistant_id, scopes)` — the scopes come from
    /// the verified claims and feed the daemon-side per-verb scope check.
    fn member_envelope_actor(
        &self,
        envelope: &str,
        verb: &str,
    ) -> Result<(String, String, String, Vec<String>, String)> {
        let config = board_identity::read_config(&self.state_dir).map_err(|_| {
            Error::rejected(format!(
                "{verb} refused: no board identity is configured to verify member authority"
            ))
        })?;
        let kid = crate::assistant_actor::peek_kid(envelope).ok_or_else(|| {
            Error::rejected(format!(
                "{verb} refused: member envelope header is not the shape"
            ))
        })?;
        let now = crate::issue::time::now_epoch();
        let key = {
            let mut cache = MEMBER_JWKS.lock().unwrap_or_else(|e| e.into_inner());
            cache.key(&config.issuer, &kid, now).map_err(|e| {
                Error::rejected(format!(
                    "{verb} refused: member envelope key check failed: {e}"
                ))
            })?
        };
        let actor = crate::assistant_actor::verify(envelope, &config, &key, now).map_err(|r| {
            Error::rejected(format!("{verb} refused: member envelope: {}", r.message))
        })?;
        Ok((
            actor.org,
            actor.sub,
            actor.assistant,
            actor.scopes,
            actor.credential_id,
        ))
    }

    /// LIVE-PULL authority revalidation (kickoff §4 — the contract
    /// requirement that a ≤300 s JWT, a provisioning-time check, or a
    /// locally-cached/stored binding is NOT revocation proof). At every
    /// protected member read/effect/resume the daemon re-asks the
    /// platform whether the member is STILL live, over the board's pinned
    /// `POST {issuer}/__platform/assistant/revalidate` (the configured
    /// issuer origin, not a host-derived destination), presenting
    /// the member's signed envelope as the bearer (the member re-proves
    /// their own `(org, sub, assistant)` — no service credential, no
    /// repurposed `results.submit` enrollment, no user impersonation) and
    /// the `{organization_id, sub, assistant_id}` triple. The platform
    /// re-reads `workspace_members` + `personalAssistants` live and
    /// answers `{allow: bool}`.
    ///
    /// FAIL CLOSED: an unreachable board, a non-200, a malformed body, or
    /// `allow:false` all refuse — a removed member's envelope still
    /// verifies (the signature is valid) but `revalidate` answers
    /// `allow:false` because the live membership row is gone.
    ///
    /// `state_dir` is the daemon's own; `board_identity::Config` supplies
    /// the board's public `host` (the audience and the hosted-board
    /// custody account) and the pinned `issuer` (the network
    /// destination). The pull is bounded (small timeout, bounded body,
    /// no redirect follow) and never logs the envelope.
    pub(super) fn member_authority_live(
        &self,
        envelope: &str,
        member: &MemberCaller,
        revalidate: MemberReprove,
        verb: &str,
    ) -> Result<()> {
        let MemberReprove {
            operation,
            scope,
            job_id,
            op_credential_id,
        } = revalidate;
        let org = member.org.as_str();
        let sub = member.sub.as_str();
        let assistant_id = member.assistant_id.as_str();
        let config = board_identity::read_config(&self.state_dir).map_err(|_| {
            Error::rejected(format!(
                "{verb} refused: no board identity configured for member authority revalidation"
            ))
        })?;
        // The wire goes to the CONFIGURED pinned issuer origin — the
        // platform that minted the session, not a destination derived
        // from the public audience host (which may not serve the
        // `__platform/*` worker directly). `config.host` remains the
        // audience/custody key only; the network destination is
        // `config.issuer`, pinned by the same `board-identity.json`
        // trust root the signature check reads. The exact contract path
        // is appended unchanged.
        let host = config.host.trim_end_matches('/');
        let issuer = config.issuer.trim_end_matches('/');
        let url = format!("{issuer}/__platform/assistant/revalidate");
        let agent = crate::issuer_transport::agent(
            ureq::Agent::config_builder()
                .timeout_global(Some(std::time::Duration::from_secs(8)))
                .http_status_as_error(false)
                .max_redirects(0),
        );
        // The daemon binds the operation + its required scope to the
        // protected verb itself — never caller-supplied — so the platform
        // re-derives `allow` from the member's CURRENT role, the live
        // issuing-credential state and grant ceiling, and the active
        // assistant binding. `resource` is server-bound to the member's
        // own assistant (`kind:"assistant"`).
        let mut resource = serde_json::Map::new();
        resource.insert("kind".to_string(), json!("assistant"));
        // Job-bound verbs propagate the actual job id so the platform
        // re-checks the resource the effect names, not a generic
        // assistant resource. `None` for the others.
        if let Some(job_id) = job_id {
            resource.insert("job_id".to_string(), json!(job_id));
        }
        // `job_dispatch` carries the queued request's own
        // `assist_requests.credential_id` — the platform re-proves
        // THIS recorded credential live, never the binding's latest pin,
        // so a stale queued effect denied after revocation does NOT
        // ride a renewal reconcile. `None` for every other op.
        if let Some(credential_id) = op_credential_id {
            resource.insert("credential_id".to_string(), json!(credential_id));
        }
        let body = serde_json::json!({
            "version": "hosted-cadence-assistant.v1",
            "organization_id": org,
            "sub": sub,
            "assistant_id": assistant_id,
            "operation": operation,
            "scope": scope,
            "resource": resource,
        });
        // Bearer: prefer the daemon's own enrolled `hosted-board` service
        // credential (least-authority `assist.revalidate` deferred-commit
        // authority, operator-enrolled via `platform_enroll`); fall back to
        // the member's signed envelope for member-initiated re-proving.
        // FAIL-CLOSED on custody: when a `hosted-board` record EXISTS but
        // its custody bytes cannot be loaded/validated, the request
        // refuses — a configured-but-unloadable service binding must never
        // silently degrade to the member path (no custody-corruption
        // fallback). Only a genuinely ABSENT record uses the member
        // envelope path.
        let bearer: String;
        match self.store.platform_credential("hosted-board", host) {
            Err(e) => {
                return Err(Error::rejected(format!(
                    "{verb} refused: the hosted-board service credential record \
                     could not be read: {e}"
                )));
            }
            Ok(Some(record)) => {
                let key = crate::platform::custody::Key {
                    platform: "hosted-board",
                    account: host,
                };
                let token = self
                    .platform_custody
                    .load(&record.custody, &key)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty());
                match token {
                    Some(t) => bearer = t,
                    None => {
                        return Err(Error::rejected(format!(
                            "{verb} refused: the configured hosted-board service \
                             credential has no loadable custody material"
                        )));
                    }
                }
            }
            Ok(None) => bearer = format!("wikienv_{envelope}"),
        }
        let mut resp = agent
            .post(&url)
            .header("content-type", "application/json")
            // Bearer selection — the daemon prefers its OWN dedicated
            // `assist.revalidate` board-service credential when the
            // operator has enrolled one into `platform_credentials`
            // custody under `platform="hosted-board"` (the least-authority
            // deferred-commit path — a stored job resuming with no fresh
            // member envelope). Absent that record, the member's signed
            // envelope is the bearer (member-initiated re-proving). Both
            // land `allow` only when the TARGET member's live role +
            // live issuing-credential + active binding all still hold.
            .header("authorization", &format!("Bearer {}", bearer))
            .send_json(body)
            .map_err(|e| {
                Error::rejected(format!(
                    "{verb} refused: member authority revalidation unreachable: {e}"
                ))
            })?;
        if resp.status() != 200 {
            return Err(Error::rejected(format!(
                "{verb} refused: member authority revalidation answered HTTP {}",
                resp.status().as_u16()
            )));
        }
        let mut bytes = Vec::new();
        resp.body_mut()
            .as_reader()
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                Error::rejected(format!("{verb} refused: revalidation body unreadable: {e}"))
            })?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            Error::rejected(format!(
                "{verb} refused: revalidation body is not the contract shape"
            ))
        })?;
        if value.get("allow").and_then(Value::as_bool) != Some(true) {
            return Err(Error::rejected(format!(
                "{verb} refused: member authority is not live at commit — \
                 the platform revalidation did not allow"
            )));
        }
        Ok(())
    }

    /// The member caller: a signed `member_envelope` re-verified at the
    /// daemon — the kickoff's "daemon-verifiable authority" rule. The
    /// member proof is the JWS itself, not a relayed field: the daemon
    /// re-checks the signature and every `iss`/`aud`/`org`/`actor`/
    /// `assistant`/`kind`/`scope` pin itself, so a forged or widened field
    /// can never mint or widen a member identity.
    ///
    /// Admission requires (a) a proven operator/board connection — the
    /// only channel allowed to carry a relayed member envelope — and (b) a
    /// member envelope that re-verifies cryptographically and re-binds to
    /// an `active` stored `assistants` row. The relayed envelope is the
    /// proof; the connection proves only that a trusted relay carried it.
    /// The bound alias is the STORED `assistants.alias` — mint-derived
    /// and member-bound, never a request field.
    ///
    /// Ensure the member's assistant exists as a REAL registered agent
    /// plus a bound private conversation. Idempotent: an existing active
    /// agent + thread is reused; a first provision registers a confined,
    /// read-only pi worker under `assist-<sub>` (a worker profile, never
    /// master, never operator authority) and binds its private home
    /// thread as the member's conversation. The daemon owns every field —
    /// nothing here reads a caller-supplied alias, install or authority.
    ///
    /// The registration records `params.confine=true` + `params.assist` so
    /// the launcher encloses it under the worker confinement policy and
    /// downstream surfaces can tell it is a member assistant, not an
    /// ordinary worker — the assistant never inherits the Master's global
    /// tool allowlist (that is profile-keyed on the master alias).
    /// The principal's dedicated confined cwd: `state_dir/assistants/<digest>`.
    /// The digest covers the canonical `(org, sub)` pair, length-prefixed so
    /// `("a|b", "c")` and `("a", "b|c")` can never encode alike. Distinct
    /// principals therefore never share one confined read/write directory.
    pub(super) fn assistant_principal_cwd(&self, org: &str, sub: &str) -> std::path::PathBuf {
        let canonical = format!("{}:{org}|{}:{sub}", org.len(), sub.len());
        self.state_dir
            .join("assistants")
            .join(crate::operator_auth::digest(&canonical))
    }

    /// CAD-1380 private-cwd invariant, shared by every path that can grant a
    /// bound runtime filesystem authority (provision, resume, open, admission).
    /// A registered runtime for an ACTIVE binding must sit exactly at its
    /// principal's dedicated root. Unsafe, missing or erroring rows refuse;
    /// nothing repoints, migrates or widens the grant. Ordinary aliases with no
    /// binding are untouched (the caller never invokes this for them).
    pub(super) fn assistant_runtime_cwd_guard(
        &self,
        binding: &crate::store::AssistantBinding,
    ) -> Result<()> {
        let live = self.store.agent_opt(&binding.alias)?;
        self.assistant_cwd_matches(binding, live.as_ref())
    }

    /// Start/resume gate for a bound alias: an ACTIVE bound binding must pass
    /// the cwd invariant before any state, enable or spawn write. Ordinary
    /// aliases (no binding) are untouched.
    pub(super) fn assistant_start_guard(&self, alias: &str) -> Result<()> {
        match self.store.assistant_for_alias(alias)? {
            Some(binding) => self.assistant_runtime_cwd_guard(&binding),
            None => Ok(()),
        }
    }

    /// The same invariant against one runtime snapshot (the row the actor is
    /// about to open). `None` means the bound runtime is missing: refuse.
    pub(super) fn assistant_cwd_matches(
        &self,
        binding: &crate::store::AssistantBinding,
        runtime: Option<&crate::store::Agent>,
    ) -> Result<()> {
        // An inactive or revoked binding never receives a filesystem grant.
        if !binding.is_active() {
            return Err(Error::rejected(format!(
                "member assist refused: runtime '{}' has an {} binding (CAD-1380)",
                binding.alias, binding.state
            )));
        }
        let expected = self.assistant_principal_cwd(&binding.org, &binding.sub);
        let Some(runtime) = runtime else {
            return Err(Error::rejected(format!(
                "member assist refused: bound runtime '{}' is not registered (CAD-1380)",
                binding.alias
            )));
        };
        if expected.to_str().is_none() || runtime.cwd != expected.to_string_lossy() {
            return Err(Error::rejected(format!(
                "member assist refused: registered runtime '{}' is not in its \
                 principal's dedicated cwd; repair it explicitly (CAD-1380)",
                binding.alias
            )));
        }
        Ok(())
    }

    fn ensure_assistant_runtime(
        &self,
        binding: &crate::store::AssistantBinding,
    ) -> Result<crate::store::AssistantBinding> {
        // Register the assistant agent lazily. Its cwd is a DEDICATED
        // principal-owned subdirectory, `state_dir/assistants/<digest>`, where
        // the digest is a collision-resistant hash of the canonical (org, sub)
        // pair. The hash keeps raw subjects out of the path, is stable across
        // credential rotation, and never truncates or lossily maps characters.
        let principal_cwd = self.assistant_principal_cwd(&binding.org, &binding.sub);
        if self.store.agent_opt(&binding.alias)?.is_some() {
            self.assistant_runtime_cwd_guard(binding)?;
            // CAD-1420: seed missing files; BOOTSTRAP only while pending.
            super::bootstrap_files::seed(&principal_cwd, binding.bootstrap_state == "pending")?;
        } else {
            // The registered alias is the STORED binding alias — derived
            // at mint from the member, never a caller field. The agent-
            // class gate (`member_assist_policy`) keys on that alias
            // family, so a registered member assistant is always caught
            // by the member boundary.
            let cwd = principal_cwd;
            std::fs::create_dir_all(&cwd).map_err(|e| {
                Error::rejected(format!(
                    "member assist refused: the assistant's principal cwd cannot be created: {e}"
                ))
            })?;
            super::bootstrap_files::seed(&cwd, binding.bootstrap_state == "pending")?;
            let params = json!({"confine": true, "assist": binding.sub});
            self.store.register_agent(&crate::store::NewAgent {
                alias: &binding.alias,
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd: cwd.to_str().unwrap_or_default(),
                sandbox: "read-only",
                instructions: Some(
                    "You are a member's personal assistant bound to ONE principal. Read, \
                     research and explain THAT member's authorized work; propose jobs for \
                     the operator/PM to approve. You cannot approve, dispatch, merge, or \
                     touch any other member's state, credentials or files.",
                ),
                params: Some(&params.to_string()),
                team_role: None,
                model_policy: None,
            })?;
        }
        // Bind the private conversation: the alias's home thread is the
        // member's own principal-private conversation. Never a shared app
        // installation or a cloned `install_id` — that would leak other
        // principals' context.
        if binding.conversation_id.is_none() {
            let thread = self.store.ensure_thread(&binding.alias)?;
            self.store
                .assistant_set_conversation(&binding.org, &binding.sub, &thread.id)?;
            return self
                .store
                .assistant_binding(&binding.org, &binding.sub)?
                .ok_or_else(|| Error::rejected("assistant binding vanished after provisioning"));
        }
        Ok(binding.clone())
    }

    /// CAD-1380 R1: re-prove a queued or admitted turn against the member
    /// authority it runs under. The immutable binding for `message.alias` is
    /// read for EVERY message, so a revoked bound assistant cannot execute any
    /// source (notifications included).
    /// - No binding and no stamp: a genuine nonmember alias, ordinary behavior.
    /// - No binding but a stamp: fail closed.
    /// - Binding not active: refuse.
    /// - Stamped member send: the stamp must match the binding tuple, then the
    ///   ORIGINAL recorded credential is re-proven for `assist_send`.
    /// - Bound, unstamped, `member-` source: fail closed (no member provenance).
    /// - Bound, unstamped, other source: the current binding is re-proven for
    ///   `assist_state` under the enrolled service. No latest CID is borrowed.
    pub(super) fn member_message_reprove(&self, message: &store::Message) -> Result<()> {
        let stamp = self.store.message_member_authority(&message.id)?;
        let binding = self.store.assistant_for_alias(&message.alias)?;
        // CAD-1380 R4: a queued assist-job notice re-proves the subscriber's
        // live jobs scope for the job its task names. Its original member
        // credential is never reused. The private-cwd invariant runs first, on
        // the bound runtime that would receive the notice.
        if message.source == "assist_job_event" {
            if let Some(b) = binding.as_ref() {
                self.assistant_runtime_cwd_guard(b)?;
            }
            return self.assist_notice_reprove(message);
        }
        let Some(binding) = binding else {
            if stamp.is_some() || message.source.starts_with("member-") {
                return Err(Error::rejected(format!(
                    "member message '{}' refused: its assistant binding is absent or its \
                     member provenance is unverified",
                    message.id
                )));
            }
            return Ok(());
        };
        if !binding.is_active() {
            return Err(Error::rejected(format!(
                "message '{}' refused: the assistant binding is {}",
                message.id, binding.state
            )));
        }
        self.assistant_runtime_cwd_guard(&binding)?;
        let member_lineage = message.source.starts_with("member-");
        match stamp {
            Some(auth) => {
                if binding.org != auth.org
                    || binding.sub != auth.sub
                    || binding.assistant_id != auth.assistant_id
                {
                    return Err(Error::rejected(format!(
                        "member message '{}' refused: its recorded provenance does not match the binding",
                        message.id
                    )));
                }
                let member = MemberCaller {
                    org: auth.org,
                    sub: auth.sub,
                    assistant_id: auth.assistant_id,
                    alias: binding.alias.clone(),
                    conversation_id: None,
                    state: binding.state.clone(),
                    credential_id: auth.credential_id.clone(),
                };
                self.member_authority_live(
                    "",
                    &member,
                    MemberReprove {
                        operation: "assist_send",
                        scope: "assist.write",
                        job_id: None,
                        op_credential_id: Some(&auth.credential_id),
                    },
                    "member message",
                )
            }
            None if member_lineage => Err(Error::rejected(format!(
                "member message '{}' has no verified member provenance — it cannot run as \
                 ordinary work",
                message.id
            ))),
            None => {
                let member = MemberCaller {
                    org: binding.org.clone(),
                    sub: binding.sub.clone(),
                    assistant_id: binding.assistant_id.clone(),
                    alias: binding.alias.clone(),
                    conversation_id: binding.conversation_id.clone(),
                    state: binding.state.clone(),
                    credential_id: String::new(),
                };
                self.member_authority_live(
                    "",
                    &member,
                    MemberReprove {
                        operation: "assist_state",
                        scope: "assist.read",
                        job_id: None,
                        op_credential_id: None,
                    },
                    "member message",
                )
            }
        }
    }

    /// CAD-1380 R4: drain pending assist-job intents (oldest first, bounded).
    /// Runs from the watch loop OUTSIDE any store transaction or connection
    /// lock. Returns the count of notices newly queued. With the rollout flag
    /// off it returns `Ok(0)` before any read, write, HTTP call or notice, so
    /// pending intents are retained. A terminal outcome is recorded once with
    /// `finish_assist_job_notification`; a store failure returns an error and
    /// the intent stays pending (never silently claimed).
    pub(super) fn route_assist_job_notifications(self: &Arc<Self>) -> Result<usize> {
        let pending = self
            .store
            .pending_assist_job_notifications(ASSIST_NOTICE_BATCH)?;
        let mut queued = 0;
        for (seq, payload) in pending {
            match self.route_assist_intent(seq, &payload) {
                Ok(Some(alias)) => {
                    queued += 1;
                    self.notify_agent(&alias);
                }
                Ok(None) => {}
                Err(error) => {
                    // An expected refusal (`Rejected`: confinement, transport
                    // denial, a changed tuple) is terminal and truthful. Any
                    // other error is a store/internal failure: it propagates
                    // and the intent stays pending, never silently claimed.
                    let Error::Rejected(reason) = &error else {
                        return Err(error);
                    };
                    let reason = bounded_reason(reason);
                    self.store
                        .finish_assist_job_notification(seq, "unresolved")?;
                    eprintln!("assist notice {seq} unresolved: {reason}");
                }
            }
        }
        Ok(queued)
    }

    /// One intent: resolve the canonical job and its owner ledger, then route
    /// by target. Returns the terminal outcome and the alias to wake after
    /// commit. Every missing or mismatched field refuses; nothing is inferred.
    fn route_assist_intent(self: &Arc<Self>, seq: i64, p: &Value) -> Result<Option<String>> {
        let refuse = |store: &Store| store.finish_assist_job_notification(seq, "refused");
        let (Some(target), Some(job), Some(task_id), Some(key), Some(state)) = (
            intent_str(p, "target"),
            intent_str(p, "job"),
            intent_str(p, "task"),
            intent_str(p, "key"),
            intent_str(p, "state"),
        ) else {
            refuse(&self.store)?;
            return Ok(None);
        };
        let Some(revision) = p.get("revision").and_then(Value::as_i64) else {
            refuse(&self.store)?;
            return Ok(None);
        };
        let note = intent_str(p, "note").unwrap_or_default();
        let body =
            format!("Cadence job {job}: task {task_id} r{revision} is now {state}. Next: {note}");
        // A domain-separated 64-hex digest keeps the message id within the
        // 64-char id limit and stays deterministic per intent key.
        let id = crate::operator_auth::digest(&format!("assist-notice:{key}"));
        let task = match self.store.task(&task_id) {
            Ok(t) if t.job_id == job => t,
            _ => {
                self.store
                    .finish_assist_job_notification(seq, "unresolved")?;
                return Ok(None);
            }
        };
        let Some(owner) = self.store.assist_request_for_job(&job)? else {
            refuse(&self.store)?;
            return Ok(None);
        };
        match target.as_str() {
            "master" => {
                if intent_str(p, "alias").as_deref() != Some(crate::master::ALIAS) {
                    refuse(&self.store)?;
                    return Ok(None);
                }
                if self.store.agent_opt(crate::master::ALIAS)?.is_none() {
                    self.store
                        .finish_assist_job_notification(seq, "unresolved")?;
                    return Ok(None);
                }
                // Master uses the normal Home report path (`send_as`), which
                // already applies the confined-Devin refusal for non-master.
                self.send_as(
                    &json!({"alias": crate::master::ALIAS, "text": body, "message": id, "source": "report"}),
                    &|_| Ok(store::Sender::Unattributed),
                )?;
                self.store
                    .finish_assist_job_notification(seq, "delivered")?;
                Ok(None)
            }
            "author" => {
                let Some(alias) = intent_str(p, "alias") else {
                    refuse(&self.store)?;
                    return Ok(None);
                };
                if state != "revising" || task.assignee.as_deref() != Some(alias.as_str()) {
                    self.store
                        .finish_assist_job_notification(seq, "unresolved")?;
                    return Ok(None);
                }
                let Some(agent) = self.store.agent_opt(&alias)? else {
                    self.store
                        .finish_assist_job_notification(seq, "unresolved")?;
                    return Ok(None);
                };
                let Some(kickoff) = self.store.task_kickoff_created(&task_id, &alias)? else {
                    self.store
                        .finish_assist_job_notification(seq, "unresolved")?;
                    return Ok(None);
                };
                if agent.created > kickoff {
                    self.store
                        .finish_assist_job_notification(seq, "unresolved")?;
                    return Ok(None);
                }
                crate::adapter::pi::refuse_confined_devin_agent(&agent)?;
                // The author is an assigned worker, not a subscriber: after
                // the checks above its notice is queued directly, and the
                // intent is finished as delivered in the same guarded step.
                let (duplicate, _) = self.store.enqueue_task(
                    &alias,
                    &body,
                    None,
                    &id,
                    "job_event",
                    Some(&task_id),
                )?;
                self.store
                    .finish_assist_job_notification(seq, "delivered")?;
                Ok((!duplicate).then_some(alias))
            }
            "subscriber" => {
                let (Some(org), Some(sub), Some(assistant_id)) = (
                    intent_str(p, "org"),
                    intent_str(p, "sub"),
                    intent_str(p, "assistant_id"),
                ) else {
                    refuse(&self.store)?;
                    return Ok(None);
                };
                if owner.org != org {
                    refuse(&self.store)?;
                    return Ok(None);
                }
                let Some(binding) = self.store.assistant_by_id(&org, &assistant_id)? else {
                    refuse(&self.store)?;
                    return Ok(None);
                };
                if !binding.is_active() || binding.sub != sub {
                    refuse(&self.store)?;
                    return Ok(None);
                }
                // Live jobs-scope proof, outside any store transaction. The
                // member's original credential is never borrowed.
                let member = MemberCaller {
                    org: org.clone(),
                    sub: sub.clone(),
                    assistant_id: binding.assistant_id.clone(),
                    alias: binding.alias.clone(),
                    conversation_id: binding.conversation_id.clone(),
                    state: binding.state.clone(),
                    credential_id: String::new(),
                };
                let proof = self.member_authority_live(
                    "",
                    &member,
                    MemberReprove {
                        operation: "job_events",
                        scope: "assist.jobs",
                        job_id: Some(&job),
                        op_credential_id: None,
                    },
                    "assist notice",
                );
                if proof.is_err() {
                    refuse(&self.store)?;
                    return Ok(None);
                }
                crate::adapter::pi::refuse_confined_devin_agent(
                    &self.agent_for_binding(&binding)?,
                )?;
                self.store.commit_assist_intent(
                    seq,
                    &self.intent_recipient(&job, &binding.alias, p)?,
                    &body,
                    &id,
                    "assist_job_event",
                    &task_id,
                )
            }
            _ => {
                refuse(&self.store)?;
                Ok(None)
            }
        }
    }

    /// The immutable recipient tuple of an intent, as the final commit checks it.
    fn intent_recipient(
        &self,
        job: &str,
        alias: &str,
        p: &Value,
    ) -> Result<store::AssistRecipient> {
        let (Some(org), Some(sub), Some(assistant_id)) = (
            intent_str(p, "org"),
            intent_str(p, "sub"),
            intent_str(p, "assistant_id"),
        ) else {
            return Err(Error::rejected("assist intent has no recipient tuple"));
        };
        Ok(store::AssistRecipient {
            alias: alias.to_string(),
            job: job.to_string(),
            org,
            sub,
            assistant_id,
        })
    }

    /// The agent row of an active binding, for the confinement refusal.
    fn agent_for_binding(&self, binding: &store::AssistantBinding) -> Result<store::Agent> {
        self.store
            .agent_opt(&binding.alias)?
            .ok_or_else(|| Error::rejected("assist notice: assistant alias is not registered"))
    }

    fn assist_notice_reprove(&self, message: &store::Message) -> Result<()> {
        let refused =
            |why: &str| Error::rejected(format!("assist notice '{}' refused: {why}", message.id));
        let task_id = message
            .task_id
            .as_deref()
            .ok_or_else(|| refused("no task"))?;
        let job = self
            .store
            .task(task_id)
            .map_err(|_| refused("task unknown"))?
            .job_id;
        let binding = self
            .store
            .assistant_for_alias(&message.alias)?
            .filter(|b| b.is_active())
            .ok_or_else(|| refused("binding inactive"))?;
        let owner_ok = self
            .store
            .assist_request_for_job(&job)?
            .is_some_and(|o| o.org == binding.org);
        let member_ok = self
            .store
            .job_is_subscriber(&job, &binding.org, &binding.sub)?
            || self
                .store
                .assist_request_owns(&binding.org, &binding.sub, &job)?;
        if !owner_ok || !member_ok {
            return Err(refused("not a subscriber of this job"));
        }
        let member = MemberCaller {
            org: binding.org.clone(),
            sub: binding.sub.clone(),
            assistant_id: binding.assistant_id.clone(),
            alias: binding.alias.clone(),
            conversation_id: binding.conversation_id.clone(),
            state: binding.state.clone(),
            credential_id: String::new(),
        };
        self.member_authority_live(
            "",
            &member,
            MemberReprove {
                operation: "job_events",
                scope: "assist.jobs",
                job_id: Some(&job),
                op_credential_id: None,
            },
            "member message",
        )
    }

    /// `member_caller` — admission for every `member_*` verb: proven
    /// operator relay, signed member envelope re-verify, per-verb scope
    /// gate, live binding re-bind, and the §4 live-authority pull bound
    /// to `operation` + its required scope.
    /// `operation`/`need` are the server-bound operation + scope the verb
    /// requires — passed to `member_authority_live` so the platform
    /// re-derives `allow` from CURRENT role + live issuing-credential
    /// state + grant ceiling + active binding, not just membership.
    pub(super) fn member_caller(
        &self,
        params: &Value,
        peer_pid: u32,
        verb: &str,
        operation: &str,
        need: crate::assistant_actor::AssistScope,
        job_id: Option<&str>,
    ) -> Result<MemberCaller> {
        // The connection itself must be the operator/board relay — the
        // only channel allowed to carry a relayed member envelope. An
        // agent or unproven connection is refused outright, before the
        // envelope is read.
        self.proven_operator(verb, peer_pid)?;
        let envelope = optional_str(params, "member_envelope").ok_or_else(|| {
            Error::rejected(format!(
                "{verb} refused: no member_envelope proof — \
                 a member operation requires a verified assistant envelope"
            ))
        })?;
        let (org, sub, assistant_id, scopes, credential_id) =
            self.member_envelope_actor(envelope, verb)?;
        // Per-verb scope gate BEFORE the live pull — an `assist.read`-
        // only envelope never reaches the commit-path authority check for
        // a write/jobs verb.
        let member_scopes = scopes.clone();
        let want_scope = match need {
            crate::assistant_actor::AssistScope::Read => "assist.read",
            crate::assistant_actor::AssistScope::Write => "assist.write",
            crate::assistant_actor::AssistScope::Jobs => "assist.jobs",
        };
        if !member_scopes.iter().any(|s| s == want_scope) {
            return Err(Error::rejected(format!(
                "{verb} refused: the member envelope's scopes do not cover {want_scope}"
            )));
        }
        let binding = self.store.assistant_binding(&org, &sub)?.ok_or_else(|| {
            Error::rejected(format!(
                "{verb} refused: no assistant binding for this member — \
                 provisioning happens through the platform mint, not a request"
            ))
        })?;
        if binding.assistant_id != assistant_id {
            return Err(Error::rejected(format!(
                "{verb} refused: member envelope names assistant '{assistant_id}', not the \
                 member's bound assistant '{}'",
                binding.assistant_id
            )));
        }
        if binding.org != org {
            return Err(Error::rejected(format!(
                "{verb} refused: member envelope org '{org}' does not match the binding"
            )));
        }
        if !binding.is_active() {
            return Err(Error::rejected(format!(
                "{verb} refused: the member's assistant binding is {} — revoked/\
                 archived membership admits no effect",
                binding.state
            )));
        }
        // §4 live pull: re-prove the member's platform authority at THIS
        // effect/commit, bound to the exact `operation`+`scope` — the
        // stored binding and the ≤300 s envelope are not revocation proof,
        // and the platform re-derives `allow` from CURRENT role + live
        // issuing-credential state + grant ceiling + active binding, not
        // just membership existence.
        let member_call = MemberCaller {
            org,
            sub,
            assistant_id,
            alias: binding.alias.clone(),
            conversation_id: binding.conversation_id.clone(),
            state: binding.state.clone(),
            credential_id,
        };
        // Member-initiated ops re-prove the VERIFIED incoming envelope's
        // credential (`member_call.credential_id`), even when a service bearer
        // is preferred. A binding's latest CID is never borrowed for this call.
        self.member_authority_live(
            envelope,
            &member_call,
            MemberReprove {
                operation,
                scope: want_scope,
                job_id,
                op_credential_id: Some(&member_call.credential_id),
            },
            verb,
        )?;
        // Lazily provision the assistant runtime + private conversation
        // once the member is proven live — a member's first verb brings up
        // their real agent and binds its private thread.
        let binding = self.ensure_assistant_runtime(&binding)?;
        let member_call = MemberCaller {
            conversation_id: binding.conversation_id,
            ..member_call
        };
        Ok(member_call)
    }

    /// The member assistant's daemon-owned alias, derived server-side from
    /// the canonical `(org, sub)` principal — the same length-prefixed digest
    /// `assistant_principal_cwd` uses, so the alias and the confined cwd key
    /// the same unambiguous principal. `assist-` + first 48 hex chars of the
    /// digest is 55 chars, inside the 64-char identifier grammar, and is
    /// deterministic across credential rotation. A request never names it.
    pub(super) fn assistant_principal_alias(&self, org: &str, sub: &str) -> String {
        let canonical = format!("{}:{org}|{}:{sub}", org.len(), sub.len());
        let digest = crate::operator_auth::digest(&canonical);
        format!("assist-{}", &digest[..48])
    }

    /// CAD-1420: `platform_assistant_provision` — the daemon's dedicated
    /// platform-authority projection. The `/api/assist/<verb>` relay invokes
    /// it BEFORE the ordinary `member_assist_state|history|send` verb so a
    /// genuinely absent local binding can be projected from the platform's
    /// already-verified signed assistant envelope — never from a request
    /// field, a member self-claim or a SQLite seed.
    ///
    /// Closed params: `member_envelope` + `operation` only. `operation` is
    /// allowlisted to the read/write member ops (`assist_state`,
    /// `assist_history`, `assist_send`); every job operation and every other
    /// field is refused — a job's provenance still comes from
    /// `member_job_request` alone. The connection must be the proven
    /// operator/board relay (`proven_operator`), and the envelope is
    /// cryptographically re-verified (`member_envelope_actor`) exactly as
    /// `member_caller` does: issuer, JWKS, audience, company, time, kind,
    /// subject, assistant and scope pins all re-check here.
    ///
    /// An existing stored binding is NOT touched: a matching active tuple
    /// returns `{"provisioned": false}` without any mutation (no freshness,
    /// enrollment or runtime write — the existing member path keeps its
    /// current behavior), and a mismatched tuple or a revoked/archived
    /// binding is refused, never renamed, revived or reset.
    ///
    /// A genuinely ABSENT binding may be projected only when the board's own
    /// enrolled `hosted-board` service credential is present and loadable —
    /// the daemon's least-authority `assist.revalidate` custody — AND the
    /// live `member_authority_live` pull (EMPTY envelope, so the service
    /// bearer is the proof and the member-envelope fallback can never apply)
    /// answers `allow` for the exact operation + scope, re-proving the
    /// member's CURRENT issuing credential (`op_credential_id` = the verified
    /// incoming envelope's credential). Then `store.assistant_bind` writes
    /// the row inside its own transaction, which atomically refuses a
    /// conflicting tuple and a pre-existing `agents.alias` collision.
    /// Nothing here registers an agent, mints a runtime/thread/file, or
    /// starts an endpoint — the lazy `ensure_assistant_runtime` inside the
    /// subsequent member verb still owns that.
    pub(super) fn rpc_platform_assistant_provision(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let verb = "platform assistant provision";
        reject_unknown_fields(params, &["member_envelope", "operation"], verb)?;
        // The same trusted channel as every member_* verb — only the
        // board's proven relay may carry a signed member envelope.
        self.proven_operator(verb, peer_pid)?;
        let envelope = optional_str(params, "member_envelope").ok_or_else(|| {
            Error::rejected(format!(
                "{verb} refused: no member_envelope proof — \
                 projection requires a verified assistant envelope"
            ))
        })?;
        let operation = required_str(params, "operation")?;
        let want_scope = match operation {
            "assist_state" | "assist_history" => "assist.read",
            "assist_send" => "assist.write",
            _ => {
                return Err(Error::rejected(format!(
                    "{verb} refused: operation '{operation}' is not provisionable — \
                     only assist_state/assist_history/assist_send project a binding"
                )))
            }
        };
        let (org, sub, assistant_id, scopes, credential_id) =
            self.member_envelope_actor(envelope, verb)?;
        if !scopes.iter().any(|s| s == want_scope) {
            return Err(Error::rejected(format!(
                "{verb} refused: the member envelope's scopes do not cover {want_scope}"
            )));
        }
        // Re-read the stored binding. An existing row is left alone: a
        // matching active tuple is an idempotent no-op (the daemon does not
        // re-enroll, re-stamp or re-fresh what already holds), a mismatched
        // or dead tuple is refused.
        if let Some(existing) = self.store.assistant_binding(&org, &sub)? {
            if existing.assistant_id != assistant_id {
                return Err(Error::rejected(format!(
                    "{verb} refused: member envelope names assistant '{assistant_id}', not \
                     the stored binding's '{}' — a conflicting tuple is never rewritten",
                    existing.assistant_id
                )));
            }
            if existing.org != org {
                return Err(Error::rejected(format!(
                    "{verb} refused: member envelope org '{org}' does not match the binding"
                )));
            }
            if !existing.is_active() {
                return Err(Error::rejected(format!(
                    "{verb} refused: the member's assistant binding is {} — \
                     revoked/archived membership is never revived by a projection",
                    existing.state
                )));
            }
            return Ok(json!({
                "provisioned": false,
                "org": org,
                "sub": sub,
                "assistant_id": assistant_id,
                "alias": existing.alias,
                "note": "an active binding already exists — nothing was projected",
            }));
        }
        // Genuinely absent: project ONLY under the daemon's own enrolled
        // hosted-board service credential. A missing or unloadable record
        // refuses here — the member-envelope bearer fallback of
        // `member_authority_live` must never substitute for a service
        // credential at first projection (no token ingestion, no enrollment
        // shortcut, no borrowed custody).
        let config = board_identity::read_config(&self.state_dir).map_err(|_| {
            Error::rejected(format!(
                "{verb} refused: no board identity is configured for provisioning"
            ))
        })?;
        let host = config.host.trim_end_matches('/').to_string();
        match self.store.platform_credential("hosted-board", &host) {
            Err(e) => {
                return Err(Error::rejected(format!(
                    "{verb} refused: the hosted-board service credential record \
                     could not be read: {e}"
                )));
            }
            Ok(None) => {
                return Err(Error::rejected(format!(
                    "{verb} refused: no enrolled hosted-board service credential for \
                     '{host}' — projection requires the board's own enrolled authority"
                )));
            }
            Ok(Some(record)) => {
                let key = crate::platform::custody::Key {
                    platform: "hosted-board",
                    account: &host,
                };
                let loadable = self
                    .platform_custody
                    .load(&record.custody, &key)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty());
                if loadable.is_none() {
                    return Err(Error::rejected(format!(
                        "{verb} refused: the enrolled hosted-board service credential \
                         has no loadable custody material"
                    )));
                }
            }
        }
        // Live pull BEFORE the new binding: the platform re-derives `allow`
        // from the member's CURRENT role, membership, grants and active
        // upstream binding — with the service credential as bearer (empty
        // envelope) and the verified incoming credential as the re-proved
        // provenance. A revoked/stale/dead credential, or a member the
        // platform no longer serves, is refused before any row exists.
        let member = MemberCaller {
            org: org.clone(),
            sub: sub.clone(),
            assistant_id: assistant_id.clone(),
            alias: self.assistant_principal_alias(&org, &sub),
            conversation_id: None,
            state: "active".to_string(),
            credential_id: credential_id.clone(),
        };
        self.member_authority_live(
            "",
            &member,
            MemberReprove {
                operation,
                scope: want_scope,
                job_id: None,
                op_credential_id: Some(&credential_id),
            },
            verb,
        )?;
        // The daemon-minted binding: `assistant_bind`'s own transaction is
        // the CAS — a conflicting tuple or a colliding ordinary agent alias
        // refuses inside it, never silently adopts.
        let binding = self
            .store
            .assistant_bind(&org, &sub, &assistant_id, &member.alias, None)?;
        Ok(json!({
            "provisioned": true,
            "org": org,
            "sub": sub,
            "assistant_id": assistant_id,
            "alias": binding.alias,
        }))
    }

    /// `member_assist_state` — the member's own assistant surface state.
    pub(super) fn rpc_member_assist_state(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(params, &["member_envelope"], "member assist state")?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member assist state",
            "assist_state",
            crate::assistant_actor::AssistScope::Read,
            None,
        )?;
        let binding = self
            .store
            .assistant_binding(&member.org, &member.sub)?
            .ok_or_else(|| Error::rejected("assistant binding vanished"))?;
        Ok(json!({
            "assistant": binding.to_json(),
            "conversation_id": member.conversation_id,
        }))
    }

    /// `member_assist_history` — a PAGED read of the member's own
    /// assistant conversation: the stored binding's thread id (never a
    /// request field) → the thread's entries after `after` (or the newest
    /// `tail` page), bounded by `limit`. A member sees ONLY their bound
    /// conversation's entries — no other principal's chat, never a forged
    /// or caller-named thread.
    pub(super) fn rpc_member_assist_history(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["member_envelope", "after", "before", "tail", "limit"],
            "member assist history",
        )?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member assist history",
            "assist_history",
            crate::assistant_actor::AssistScope::Read,
            None,
        )?;
        let conv = member.conversation_id.clone().ok_or_else(|| {
            Error::rejected("the member's assistant has no bound conversation yet")
        })?;
        // The bound thread must still be this assistant alias's and
        // unarchived — a foreign or archived thread is never read.
        let thread = self
            .store
            .thread_by_id(&conv)?
            .filter(|t| t.alias == member.alias && !t.archived)
            .ok_or_else(|| Error::rejected("the bound conversation is not this assistant's"))?;
        let limit = optional_i64(params, "limit")
            .unwrap_or(100)
            .clamp(1, store::THREAD_PAGE_MAX);
        let tail = params.get("tail").and_then(Value::as_bool).unwrap_or(false);
        let before = optional_i64(params, "before");
        if tail || before.is_some() {
            let (entries, more) = self
                .store
                .thread_entries_before_of(&thread.id, before, limit)?;
            let cursor = entries.last().map(|e| e.seq).unwrap_or(0);
            let entries = operator_only_entries_hidden(entries);
            return Ok(json!({
                "conversation": thread.to_json(),
                "entries": entries.iter().map(store::ThreadEntry::to_json).collect::<Vec<_>>(),
                "cursor": cursor,
                "more_before": more,
            }));
        }
        let after = optional_i64(params, "after").unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("history cursor must be nonnegative"));
        }
        let entries = self.store.thread_entries_of(&thread.id, after, limit)?;
        // The cursor advances past hidden entries too, so a page that is
        // all artifact cards never stalls the member's poll.
        let cursor = entries.last().map(|e| e.seq).unwrap_or(after);
        let entries = operator_only_entries_hidden(entries);
        Ok(json!({
            "conversation": thread.to_json(),
            "entries": entries.iter().map(store::ThreadEntry::to_json).collect::<Vec<_>>(),
            "cursor": cursor,
        }))
    }

    /// `member_assist_send` — the member sends to their own assistant's
    /// bound conversation only; the target alias + conversation are the
    /// stored binding's, never a request field.
    pub(super) fn rpc_member_assist_send(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["member_envelope", "text", "message"],
            "member assist send",
        )?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member assist send",
            "assist_send",
            crate::assistant_actor::AssistScope::Write,
            None,
        )?;
        let text = required_str(params, "text")?;
        if text.trim().is_empty() {
            return Err(Error::rejected("member assist send text is empty"));
        }
        // An optional client-supplied message id — the board's pending
        // chat row carries it so the stored entry retires the pending
        // bubble, and a retried send dedupes on the same id (the store's
        // enqueue is idempotent: same id + same body is a duplicate,
        // same id + different body a conflict). The id is validated to
        // the identifier grammar; a caller never names a `sys-` id.
        let message = match optional_str(params, "message") {
            Some(id) => {
                let id = proto::identifier(id, "Message ID")?;
                if id.starts_with(proto::DAEMON_MESSAGE_PREFIX) {
                    return Err(Error::rejected(
                        "a member send may not name a daemon-owned message id",
                    ));
                }
                id
            }
            None => Uuid::new_v4().simple().to_string(),
        };
        // The send lands in the assistant alias's own thread, attributed to
        // the MEMBER (`Sender::Member(sub)`) — never `Sender::OperatorChat`,
        // which would launder the member's text into operator authority and
        // could carry an app binding. The conversation is the bound thread;
        // a member supplies no `app`/`conversation` selector.
        let send = json!({
            "alias": member.alias,
            "text": text,
            "message": message,
            "source": format!("member-{}", member.sub),
        });
        let mut receipt = self.send_as(&send, &|_| {
            Ok(store::Sender::VerifiedMember(store::MemberAuthority {
                org: member.org.clone(),
                sub: member.sub.clone(),
                assistant_id: member.assistant_id.clone(),
                credential_id: member.credential_id.clone(),
            }))
        })?;
        // The send is accepted and queued. Now bring up the assistant's
        // own managed endpoint so a real confined turn drains it — the
        // ONLY member verb that may launch, for the ONE bound alias the
        // verified envelope + live authority just proved. A launch
        // refusal never fails the send: the row stays queued and the
        // receipt names the truthful cause.
        if let Err(e) = self.member_assist_launch(&member) {
            receipt["queued"] = json!(true);
            receipt["attention"] = json!(format!("assistant endpoint did not start: {e}"));
        }
        Ok(receipt)
    }

    /// Lazily start the verified member's own assistant endpoint after a
    /// live `assist_send` admission. The alias is the stored binding's —
    /// never a caller field; the agent row is the confined `pi`/`managed`
    /// record the daemon's own `ensure_assistant_runtime` wrote, so no
    /// request picks the provider, cwd, or launch params.
    ///
    /// Launch posture (daemon-derived only):
    /// - the model is resolved from the operator's `[pi]` policy for the
    ///   `worker` role — `[pi].models.default.worker`, strictly gated by
    ///   `require_allowed` (no platform-fallback bypass), stamped onto
    ///   the row only when the agent still carries none. A confined
    ///   `devin/*` pick refuses at `refuse_confined_devin` — the same
    ///   precondition `agent_register` enforces.
    /// - an operator-stopped assistant stays stopped (CAD-1380): refused
    ///   before the model stamp, and again under the lifecycle lock inside
    ///   `try_resume_assistant` before the start claim. A timer stop stays
    ///   eligible; a fresh registration is enabled. The lifecycle lock makes
    ///   concurrent sends launch at most once; an unreconciled `unknown`
    ///   fence refuses — a fenced assistant stays fenced, the queued row
    ///   waiting for the operator reconcile.
    ///
    /// Returns `Err` (truthful, bounded — never a fabricated reply) when
    /// the endpoint could not start; the caller reports it on the
    /// receipt while the message remains queued. `Ok(())` means an
    /// actor now owns the alias (it may still fence on `open` — that
    /// lands `attention` like every managed launch).
    fn member_assist_launch(self: &Arc<Self>, member: &MemberCaller) -> Result<()> {
        let alias = &member.alias;
        // Fail closed on the fence first — an unreconciled unknown means
        // the endpoint may hold an uncertain turn; never relaunch it.
        if self.store.has_unknown(alias)? {
            return Err(Error::rejected(format!(
                "assistant '{alias}' is fenced by an unreconciled unknown outcome — \
                 the operator reconciles before it runs again"
            )));
        }
        // An operator-stopped assistant stays stopped: refuse before any
        // model stamp, so a paused row is never relaunched by a send.
        if self.assistant_stopped_by_operator(alias)? {
            return Err(Error::rejected(format!(
                "assistant '{alias}' was stopped by an operator and stays stopped — \
                 the operator resumes it; the message stays queued"
            )));
        }
        // At most one launch across concurrent sends: an owned alias is
        // success, not a refusal — its live actor drains the queue.
        if self.lifecycle.lock().unwrap().owned(alias) {
            return Ok(());
        }
        // Stamp the launch model if the agent still carries none — the
        // registration writes no model so this first write happens here,
        // before `open` (which refuses an empty pi model). Daemon-
        // resolved, operator-allowlisted, `set_params` re-derives the
        // model_selection provenance honestly. A caller can never name
        // it: `member_assist_send` carries only `member_envelope`+`text`.
        let agent = self.store.agent(alias)?;
        let has_model = agent
            .params
            .as_ref()
            .and_then(|p| p.get("model"))
            .and_then(Value::as_str)
            .is_some_and(|m| !m.trim().is_empty());
        if !has_model {
            let pi_policy = self.stored_pi_policy()?;
            let model = crate::pi_policy::resolve_model(pi_policy.as_ref(), "worker", None)
                .map_err(|e| {
                    Error::rejected(format!(
                        "assistant endpoint has no permitted pi worker model configured \
                         ([pi].models): {e}"
                    ))
                })?;
            // A confined assistant may not run a Devin model — pi-devin
            // needs the operator's credential Landlock cannot grant.
            crate::adapter::pi::refuse_confined_devin(alias, true, &model)?;
            self.store
                .set_params(alias, &json!({"model": model}))
                .map_err(|e| {
                    Error::rejected(format!("assistant launch model could not be recorded: {e}"))
                })?;
        }
        // Start the real managed actor under the lifecycle lock. A refusal
        // surfaces truthfully; a concurrent send that won the launch shows
        // up as the owned case there, not as a failure.
        self.try_resume_assistant(alias)
    }

    /// `member_job_request` — bounded shared-job request, idempotent on
    /// `(org, sub, request_key)`. The canonical job id is derived from
    /// `request_key` — NOT the wall clock — so two DISTINCT request_keys
    /// (even in the same second) mint two distinct jobs, while a replayed
    /// or concurrent equivalent on the SAME request_key returns the one
    /// canonical job (acceptance: idempotency is same-request-key, not
    /// same-second).
    ///
    /// A real `plans::Job` is created: `pm_alias` is the member's own
    /// bound assistant alias (the assistant REQUESTS and follows the
    /// job — it is NOT an authorized approver/dispatcher), `state`
    /// `open`, and a draft task carries the member's title/acceptance.
    /// The request persists its title/body/acceptance + authorization
    /// provenance so the deferred commit can re-prove the member's
    /// authority before any dispatch lands (SEC-002 / RISK-001). Nothing
    /// auto-dispatches: `plan_gate_task` and the operator/PM dispatch
    /// rules still gate the kickoff — a member cannot self-approve.
    pub(super) fn rpc_member_job_request(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &[
                "member_envelope",
                "title",
                "request_key",
                "body",
                "acceptance",
            ],
            "member job request",
        )?;
        // One publication section per daemon: authority is re-proven and
        // the request ledger re-read only after the lock, so a waiter sees
        // the committed winner. Poison fails closed.
        let _publication = self
            .member_job_lock
            .lock()
            .map_err(|_| Error::internal("member job publication lock poisoned"))?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job request",
            "job_request",
            crate::assistant_actor::AssistScope::Jobs,
            None,
        )?;
        let title = required_str(params, "title")?;
        let request_key = required_str(params, "request_key")?;
        if title.trim().is_empty() || request_key.trim().is_empty() {
            return Err(Error::rejected(
                "member job request needs a nonempty title and request_key",
            ));
        }
        if request_key.len() > 200
            || !request_key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        {
            return Err(Error::rejected(
                "request_key must be <= 200 chars of the id grammar",
            ));
        }
        let body = optional_str(params, "body").unwrap_or_default();
        let acceptance = optional_str(params, "acceptance").unwrap_or_default();
        // Idempotent: the stored (org,sub,request_key) → job mapping wins.
        if let Some(job) = self
            .store
            .assist_request_job(&member.org, &member.sub, request_key)?
        {
            let existing = self.store.job(&job).ok().map(|j| j.to_json());
            return Ok(json!({
                "job": job,
                "canonical": true,
                "requested_by": member.sub,
                "status": "existing",
                "job_record": existing,
            }));
        }
        // The canonical job id is unique per (org,sub,request_key):
        // derived from the request_key digest so distinct request_keys in
        // the same second are distinct jobs. `assist_request_bind`'s
        // INSERT-OR-IGNORE + read-back makes the concurrent equivalent
        // resolve to the committed winner — one lane per request_key.
        let key_digest =
            crate::operator_auth::digest(&format!("{}:{}:{}", member.org, member.sub, request_key));
        let job_id = format!("assist-{}", &key_digest[..key_digest.len().min(24)]);
        // Persist the request's authorization provenance BEFORE creating
        // the job — the deferred commit re-proves exactly this.
        // A job row with no ledger row is a prior partial request. Refuse
        // before any shared-file write so its committed spec is untouched.
        if self.store.job(&job_id).is_ok() {
            return Err(Error::rejected(
                "member job request is partially recorded; no artifact was changed",
            ));
        }
        let spec_path = self.assist_job_spec(&job_id, title, body, acceptance)?;
        let spec_sha = sha256_file(std::path::Path::new(&spec_path))
            .ok_or_else(|| Error::internal("assist job spec could not be hashed"))?;
        // Create the real job bound to the assistant alias as its PM
        // (the assistant tracks, never auto-dispatches). No issue_id —
        // the member cannot mint a tracker lane; an approved promotion
        // binds one through the normal PM/operator path.
        let (_dup, _job) = self.store.create_job(
            &job_id,
            Some(title),
            &spec_path,
            &spec_sha,
            &member.alias,
            None,
            None,
            None,
            2,
            None,
            Some(title),
            None,
            None,
            None,
            None, // no assignee: a draft task awaits the operator's dispatch
            if acceptance.is_empty() {
                None
            } else {
                Some(acceptance)
            },
        )?;
        let bound = self.store.assist_request_bind(
            &member.org,
            &member.sub,
            request_key,
            &job_id,
            title,
            body,
            acceptance,
            &member.credential_id,
        )?;
        // The requester auto-subscribes to their own canonical job —
        // the subscriber ACL covers the requester too.
        self.store
            .job_subscribe(&bound, &member.org, &member.sub, &member.assistant_id)?;
        // CAD-1380 R4: the inert awaiting-approval notice, after bind and the
        // subscription. Idempotent on a retry; never approves or dispatches.
        self.store.assist_request_notice(&bound)?;
        let record = self.store.job(&bound).ok().map(|j| j.to_json());
        Ok(json!({
            "job": bound,
            "canonical": bound == job_id,
            "requested_by": member.sub,
            "assistant": member.assistant_id,
            "status": "awaiting_approval",
            "job_record": record,
            "note": "the request is recorded for operator/PM approval — \
                     it dispatches no work itself",
        }))
    }

    /// Write the member's job request to a bounded spec file the job
    /// records — daemon-owned storage, never a caller path. The spec
    /// carries the title/body/acceptance verbatim so the kickoff reads
    /// exactly what the member asked for.
    fn assist_job_spec(
        &self,
        job_id: &str,
        title: &str,
        body: &str,
        acceptance: &str,
    ) -> Result<String> {
        let dir = self.state_dir.join("assist_jobs");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{job_id}.md"));
        let text = format!("# {title}\n\n{body}\n\n## Acceptance\n\n{acceptance}\n");
        // Same redaction a stored chat body gets — a member's free text is
        // scanned before it becomes a job spec.
        let redacted = match crate::secret::redact_text(&text) {
            Ok(r) => r,
            Err(_) => "[withheld: secret scan unavailable]".to_string(),
        };
        let mut bounded = redacted;
        if bounded.len() > 48_000 {
            let mut cut = 48_000;
            while cut > 0 && !bounded.is_char_boundary(cut) {
                cut -= 1;
            }
            bounded.truncate(cut);
        }
        std::fs::write(&path, bounded)?;
        Ok(path.to_string_lossy().to_string())
    }

    /// `member_job_list` — the jobs this member owns or is subscribed to.
    pub(super) fn rpc_member_job_list(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(params, &["member_envelope"], "member job list")?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job list",
            "job_list",
            crate::assistant_actor::AssistScope::Jobs,
            None,
        )?;
        let jobs = self.store.assist_member_jobs(&member.org, &member.sub)?;
        Ok(json!({ "jobs": jobs }))
    }

    /// `member_job_evidence` — revision-bound evidence for a bound job,
    /// only for the owner or a subscriber. Evidence states
    /// `reported`/`inspected`/`independently_verified`/`unverified`/
    /// `unavailable` stay distinct and bind to the actual submitted
    /// revision, never self-report.
    pub(super) fn rpc_member_job_evidence(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(params, &["member_envelope", "job"], "member job evidence")?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job evidence",
            "job_evidence",
            crate::assistant_actor::AssistScope::Jobs,
            params.get("job").and_then(Value::as_str),
        )?;
        let job = required_str(params, "job")?;
        let owns = self
            .store
            .assist_request_owns(&member.org, &member.sub, job)?;
        let sub = self
            .store
            .job_is_subscriber(job, &member.org, &member.sub)?;
        if !(owns || sub) {
            return Err(Error::rejected(format!(
                "member job evidence refused: '{job}' is not a job this member \
                 owns or subscribes to"
            )));
        }
        // REAL evidence, revision-bound. A task's state derives only from its
        // server-recorded `head_sha` and the verdict current for its revision;
        // a verdict counts only when it names that exact head. `reported` =
        // no submitted head, or a head with no matching verdict. `unavailable`
        // = the submitted commit is not present in the recorded worktree, even
        // when a matching verdict exists (the verdict stays in `verdict` as
        // audit data only). `inspected` = a revise/blocked verdict on a present
        // artifact; `independently_verified` = a pass on a present artifact.
        // Nothing here fabricates a pass.
        let job_row = match self.store.job(job) {
            Ok(j) => j,
            Err(_) => {
                return Ok(json!({
                    "job": job,
                    "evidence_state": "unavailable",
                    "revision": Value::Null,
                    "note": "the job record is not resolvable",
                }))
            }
        };
        // Artifact custody: the submitted commit must be an object in the
        // task's server-recorded worktree (objects are shared with the repo).
        // Fixed argv, no shell; the sha is re-validated, and a relative
        // worktree resolves only against the job's recorded repo, as the CLI
        // verifier does. Any missing piece counts as not present.
        let commit_present = |repo: Option<&str>, worktree: Option<&str>, head: &str| -> bool {
            let Ok(sha) = store::check_commit_sha(head) else {
                return false;
            };
            let Some(recorded) = worktree else {
                return false;
            };
            let dir = if std::path::Path::new(recorded).is_absolute() {
                std::path::PathBuf::from(recorded)
            } else {
                match repo {
                    Some(repo) => {
                        crate::worktree::layout::worktree_dir(std::path::Path::new(repo), recorded)
                    }
                    None => return false,
                }
            };
            if !dir.is_dir() {
                return false;
            }
            let mut cmd = std::process::Command::new("git");
            // Local, read-only presence only: never lazily fetch a missing
            // object from a promisor remote, and never prompt for credentials.
            cmd.args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
                .current_dir(&dir)
                .env("GIT_NO_LAZY_FETCH", "1")
                .env("GIT_TERMINAL_PROMPT", "0");
            matches!(
                crate::proc::run_bounded(&mut cmd, std::time::Duration::from_secs(10)),
                Ok(out) if out.status.success()
            )
        };
        let tasks = self.store.tasks_for_job(job)?;
        // The job's evidence is the WEAKEST of its tasks, so one unsubmitted
        // or unavailable task keeps the whole job from reading as verified.
        // A job with no tasks is request-only evidence (`reported`).
        const ORDER: [&str; 4] = [
            "unavailable",
            "reported",
            "inspected",
            "independently_verified",
        ];
        let rank = |s: &str| ORDER.iter().position(|o| *o == s).unwrap_or(0);
        let mut any_state = if tasks.is_empty() {
            "reported"
        } else {
            "independently_verified"
        };
        let mut task_evidence = Vec::new();
        for task in &tasks {
            let verdicts = self.store.verdicts_for_task(&task.id)?;
            let current = store::current_verdict(task.revision, &verdicts);
            let artifact = task.head_sha.as_deref().map(|head| {
                commit_present(job_row.repo.as_deref(), task.worktree.as_deref(), head)
            });
            let (state, sha, reviewer) = match (task.head_sha.as_deref(), artifact, current) {
                (None, _, _) => ("reported", None, None),
                (Some(head), Some(false), _) => ("unavailable", Some(head.to_string()), None),
                (Some(head), _, Some(v)) if v.sha == head => (
                    match v.verdict.as_str() {
                        "pass" => "independently_verified",
                        _ => "inspected",
                    },
                    Some(head.to_string()),
                    Some(v.reviewer.clone()),
                ),
                (Some(head), _, _) => ("reported", Some(head.to_string()), None),
            };
            if rank(state) < rank(any_state) {
                any_state = state;
            }
            task_evidence.push(json!({
                "task": task.id,
                "state": task.state,
                "revision": task.revision,
                "head_sha": task.head_sha,
                "evidence_state": state,
                "artifact_available": artifact,
                "verdict": current.map(|v| v.to_json()),
                "reviewer": reviewer,
                "sha": sha,
            }));
        }
        let revision = tasks.iter().map(|t| t.revision).max().unwrap_or(0);
        // The current pinned authority revision — the value a
        // `member_job_reconcile` must name. It is re-derived from the
        // job's CURRENT `updated` watermark on every read, so a member
        // always sees the revision covering the queued effect as it
        // stands NOW (a moved job yields a different rev; a stale-viewed
        // reconcile then refuses).
        let authority_rev = self
            .store
            .assist_job_authority_rev(job)?
            .unwrap_or_default();
        Ok(json!({
            "job": job,
            "job_state": job_row.state,
            "job_title": job_row.title,
            "evidence_state": any_state,
            "authority_rev": authority_rev,
            "revision": revision,
            "tasks": task_evidence,
            "note": "evidence binds to the actual submitted revision and verdict; \
                     a member read never fabricates verification",
        }))
    }

    /// `member_job_events` — the subscriber-visible event feed for a bound
    /// job: the job's own event stream (plan/task/dispatch/verdict events),
    /// never private chat text. Same ACL as `member_job_evidence`.
    pub(super) fn rpc_member_job_events(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["member_envelope", "job", "after", "tail"],
            "member job events",
        )?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job events",
            "job_events",
            crate::assistant_actor::AssistScope::Jobs,
            params.get("job").and_then(Value::as_str),
        )?;
        let job = required_str(params, "job")?;
        let sub = self
            .store
            .job_is_subscriber(job, &member.org, &member.sub)?;
        let owns = self
            .store
            .assist_request_owns(&member.org, &member.sub, job)?;
        if !(sub || owns) {
            return Err(Error::rejected(format!(
                "member job events refused: '{job}' is not a job this member \
                 owns or subscribes to"
            )));
        }
        // The job's own event stream — work metadata only (kind, revision,
        // task, verdict), never a member's private conversation.
        let after = optional_i64(params, "after").unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("event cursor must be nonnegative"));
        }
        if params.get("tail").and_then(Value::as_bool).unwrap_or(false) {
            let mut events = self.store.job_events_tail(job, 51)?;
            let has_older = events.len() > 50;
            if has_older {
                events.remove(0);
            }
            return Ok(json!({
                "job": job,
                "events": events.iter().map(store::Event::to_json).collect::<Vec<_>>(),
                "cursor": events.last().map(|e| e.seq).unwrap_or(0),
                "has_older": has_older,
            }));
        }
        let events = self.store.job_events(job, after, 200)?;
        Ok(json!({
            "job": job,
            "events": events.iter().map(store::Event::to_json).collect::<Vec<_>>(),
            "cursor": events.last().map(|e| e.seq).unwrap_or(after),
        }))
    }

    /// `member_job_share` — the OWNING member enables or disables
    /// sharing of their own assist-bound job inside their org (the
    /// entitlement a second member's `member_job_subscribe` requires).
    /// Only the member who created the job through `member_job_request`
    /// (`assist_request_owns`) may share it — a second member can never
    /// mark someone else's job shared, and a share on a job bound to a
    /// different org is impossible because `owns` is org+sub-bound.
    /// `enabled` (default true) writes or clears the
    /// `assist_job_shares(job, org)` row.
    pub(super) fn rpc_member_job_share(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["member_envelope", "job", "enabled"],
            "member job share",
        )?;
        let job = required_str(params, "job")?;
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let member = self.member_caller(
            params,
            peer_pid,
            "member job share",
            "job_share",
            crate::assistant_actor::AssistScope::Jobs,
            Some(job),
        )?;
        if !self
            .store
            .assist_request_owns(&member.org, &member.sub, job)?
        {
            return Err(Error::rejected(format!(
                "member job share refused: '{job}' is not a job this member owns"
            )));
        }
        self.store
            .assist_job_share(job, &member.org, &member.sub, enabled)?;
        Ok(json!({
            "job": job,
            "shared": enabled,
            "shared_by": member.sub,
            "note": if enabled {
                "the job is now shareable inside the member's organization — \
                 a second member may subscribe"
            } else {
                "sharing is off — no NEW subscription is admitted (existing \
                 subscribers are not retro-removed)"
            },
        }))
    }

    /// `member_job_reconcile {job, expected_authority_rev}` — the
    /// OWNING member's explicit job-specific authorized reconciliation:
    /// re-pin a deferred assist job's recorded provenance
    /// (`assist_requests.credential_id`) to the member's CURRENT live
    /// credential (the signed envelope's `credential_id` claim —
    /// `member_caller` has already re-proven that credential live for
    /// THIS call). Only the member who owns the job
    /// (`assist_request_owns`) may reconcile it — the recorded
    /// provenance NEVER migrates silently at mint or dispatch.
    ///
    /// `expected_authority_rev` is a REQUIRED pin — the exact revision
    /// the member observed for this canonical request (`member_job_
    /// evidence` reports it as `authority_rev`, bound to the job's
    /// current `updated` watermark so it covers the queued effect's
    /// operation/task revision). The compare-and-update runs in ONE
    /// store transaction: a stale rev, a wrong-owner rev, or a
    /// mismatched job refuses with NO change — an old viewed decision
    /// can never reauthorize a new queued effect, and a replayed or
    /// out-of-order reconcile never silently re-pins.
    ///
    /// After a reconcile the deferred dispatch carries the NEW
    /// `credential_id` — `member_authority_live`'s `job_dispatch`
    /// re-prove then re-checks THAT credential live. A queued effect
    /// whose own credential is dead stays denied until THIS verb runs.
    pub(super) fn rpc_member_job_reconcile(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["member_envelope", "job", "expected_authority_rev"],
            "member job reconcile",
        )?;
        let job = required_str(params, "job")?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job reconcile",
            "job_reconcile",
            crate::assistant_actor::AssistScope::Jobs,
            Some(job),
        )?;
        if !self
            .store
            .assist_request_owns(&member.org, &member.sub, job)?
        {
            return Err(Error::rejected(format!(
                "member job reconcile refused: '{job}' is not a job this member owns"
            )));
        }
        // The REQUIRED revision pin is checked only after ownership — a
        // non-owner learns nothing about the rev grammar, and an owner
        // without the current rev is refused before any write.
        let expected_rev = required_str(params, "expected_authority_rev").map_err(|_| {
            Error::rejected(
                "member job reconcile refused: expected_authority_rev is required — \
                 pin the job's current committed revision",
            )
        })?;
        if expected_rev.len() > 64
            || !expected_rev
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(Error::rejected(
                "expected_authority_rev must be a short revision token",
            ));
        }
        // Re-pin the recorded provenance to the member's CURRENT live
        // credential (the envelope this call carried) — bound to the
        // exact revision the member named, compared inside the same
        // transaction that writes: a stale or wrong rev refuses with no
        // change. The deferred dispatch re-prove names this exact
        // credential — a queued effect that was denied under the old
        // credential now commits under the member's proven CURRENT
        // authority, explicitly authorized by the owner.
        let new_rev = self.store.assist_request_reconcile(
            &member.org,
            &member.sub,
            job,
            &member.credential_id,
            expected_rev,
        )?;
        Ok(json!({
            "job": job,
            "reconciled": member.sub,
            "credential_id": member.credential_id,
            "authority_rev": new_rev,
            "note": "the job's deferred effect now carries the member's \
                     current provenance — the deferred dispatch re-prove \
                     names this credential",
        }))
    }

    /// `member_job_subscribe` — a SECOND member subscribes to a shared
    /// assist-bound job in their own org (AOS-216 #3 / CAD-1380 #3):
    /// "an authorized second member can subscribe to the same shared job
    /// without obtaining the first member's private conversation or
    /// creating duplicate execution". The subscription is the read-ACL
    /// grant — after it, `member_job_evidence`/`member_job_events` admit
    /// the member for THIS job (and the supervisor's `assist_stall`
    /// fan-out reaches their bound assistant alias).
    ///
    /// Entitlement (SEC-002): org membership + `assist.jobs` scope is
    /// NOT enough — the job's OWNING member must have explicitly shared
    /// it via `member_job_share` (an `assist_job_shares` row in the
    /// member's org). A member can never mint the entitlement by naming
    /// a job id; the `job` is a resource selector, never an authority.
    ///
    /// Refusals: a job that is NOT assist-bound (no `assist_requests`
    /// row — an ordinary/foreign lane can never become a member-visible
    /// share), an assist job bound to a DIFFERENT org (cross-org leak),
    /// or a same-org assist job the owner has NOT shared.
    pub(super) fn rpc_member_job_subscribe(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(params, &["member_envelope", "job"], "member job subscribe")?;
        let job = required_str(params, "job")?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job subscribe",
            "job_subscribe",
            crate::assistant_actor::AssistScope::Jobs,
            Some(job),
        )?;
        // The job must be an assist-bound job in THIS member's org. A
        // missing `assist_requests` row (not assist-bound) refuses; a row
        // naming a different org refuses — a member can only subscribe
        // inside their own organization's shared work.
        let assist = self.store.assist_request_for_job(job)?.ok_or_else(|| {
            Error::rejected(format!(
                "member job subscribe refused: '{job}' is not an assist-bound job"
            ))
        })?;
        if assist.org != member.org {
            return Err(Error::rejected(format!(
                "member job subscribe refused: '{job}' is bound to another organization"
            )));
        }
        // The job record must exist — a subscribe never names a
        // non-existent or stale id.
        let record = self.store.job(job).ok().map(|j| j.to_json());
        if record.is_none() {
            return Err(Error::rejected(format!(
                "member job subscribe refused: '{job}' has no job record"
            )));
        }
        // The entitlement: the job's owner must have explicitly shared
        // it inside THIS member's org. Org membership + `assist.jobs`
        // never substitutes for the owner's authenticated grant.
        if !self.store.assist_job_is_shared(job, &member.org)? {
            return Err(Error::rejected(format!(
                "member job subscribe refused: '{job}' is not shared by its owner"
            )));
        }
        self.store
            .job_subscribe(job, &member.org, &member.sub, &member.assistant_id)?;
        Ok(json!({
            "job": job,
            "subscribed": member.sub,
            "assistant": member.assistant_id,
            "job_record": record,
            "note": "the member now follows this shared job — evidence and \
                     events bind to it; no private conversation is shared",
        }))
    }

    /// CAD-1420: the private profile context for a stamped, interactive member
    /// turn. Returns `Ok(None)` for ordinary agents, Master, job notices, nudges,
    /// unstamped work and inactive bindings. An unknown stored state or an
    /// unreadable profile file is an error, never a silent empty profile. The
    /// BOOTSTRAP body is included only while the stored state is `pending`. The
    /// ACK contract is shown in every valid state, so a deferred or completed
    /// member can still resume or update through the same closed object.
    pub(super) fn member_profile_context(
        &self,
        message: &store::Message,
    ) -> Result<Option<String>> {
        if message.source == "assist_job_event" || message.is_nudge() {
            return Ok(None);
        }
        let Some(stamp) = self.store.message_member_authority(&message.id)? else {
            return Ok(None);
        };
        let Some(binding) = self.store.assistant_for_alias(&message.alias)? else {
            return Ok(None);
        };
        if !binding.is_active()
            || binding.org != stamp.org
            || binding.sub != stamp.sub
            || binding.assistant_id != stamp.assistant_id
        {
            return Ok(None);
        }
        let state = binding.bootstrap_state.as_str();
        let allowed: &str = match state {
            "pending" => "\"update\", \"complete\", \"skip\", \"defer\"",
            "deferred" => "\"update\", \"resume\"",
            "complete" | "skipped" => "\"update\"",
            other => {
                return Err(Error::rejected(format!(
                "stored bootstrap state '{other}' is unknown; the assistant profile is not loaded"
            )))
            }
        };
        let root = self.assistant_principal_cwd(&binding.org, &binding.sub);
        let mut out = String::from("## Your private assistant profile\n\n");
        for name in [
            super::bootstrap_files::IDENTITY,
            super::bootstrap_files::SOUL,
            super::bootstrap_files::USER,
        ] {
            out.push_str(&super::bootstrap_files::read_profile_file(&root, name)?);
            out.push_str("\n\n");
        }
        out.push_str(&format!(
            "## Profile state\n\nstate: {state}\nrevision: {rev}\n\n\
             Your reply already answers the user's request above. Setup is optional.\n\n\
             To record an agreed change on this turn, ACK with a JSON object on stdin, \
             for example: `cadence message ack --bootstrap-file -` with \
             {{\"action\":\"update\",\"expected_revision\":{rev},\"profile\":{{\"name\":\"Ana\"}}}}\n\
             Allowed actions in this state: {allowed}. Profile keys: name, tone, preferences.\n\n",
            rev = binding.bootstrap_revision,
        ));
        if state == "pending" {
            out.push_str("## Bootstrap (pending)\n\n");
            out.push_str(&super::bootstrap_files::read_profile_file(
                &root,
                super::bootstrap_files::BOOTSTRAP,
            )?);
            out.push_str("\n\n");
        }
        Ok(Some(out))
    }
}

/// CAD-1726: artifact cards are the operator's; a member reader never sees
/// one (an allowlist of what a member may read, not a hide list of titles).
fn operator_only_entries_hidden(entries: Vec<store::ThreadEntry>) -> Vec<store::ThreadEntry> {
    entries
        .into_iter()
        .filter(|e| e.kind != store::KIND_ARTIFACT)
        .collect()
}
