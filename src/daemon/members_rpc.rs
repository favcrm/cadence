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

use super::job_events_opaque;
use super::scoped_cursor::{CursorBinding, CursorDirection};
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

/// Missing, unreadable, ambiguous and already-claimed issues share a refusal
/// that names neither the issue nor any existing job.
const MEMBER_JOB_ISSUE_UNAVAILABLE: &str =
    "member job request refused: the requested issue is unavailable";

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

/// CAD-2215: pending accepted requests visited per autonomous promotion
/// sweep. Bounded so one tick never holds the publication lock or a store
/// connection over an unbounded scan.
pub(super) const ASSIST_REQUEST_BATCH: usize = 20;

/// One autonomous promotion outcome, keyed by the owner-facing request id.
/// The sweep returns these so the watch log and an independent test can
/// witness every attempt (`created`/`pending`/`refused`/`failed`) without
/// elapsed-time assumptions. This is the production trigger's return
/// value, not a test-only side channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AssistPromotion {
    pub request_id: String,
    /// `created`, `pending`, `refused` or `failed`.
    pub outcome: &'static str,
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
            return self.member_owner_or_operator(peer_pid, Some(&binding), &binding.org, None);
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
        if !self.store.assist_job_has_request(&job)? {
            return Ok(());
        }
        // Queue-only, incomplete or ambiguous provenance is still private,
        // but it has no owner we can safely authorize. The operator retains
        // recovery access; no agent may treat it as an ordinary project job.
        let Some((owner_org, owner_sub)) = self.store.assist_job_owner(&job)? else {
            return match self.connection_caller(peer_pid)? {
                caller_rule::Who::Operator => Ok(()),
                _ => match task.as_deref() {
                    Some(task) => Err(Error::rejected(format!("No such task '{task}'"))),
                    None => Err(Error::rejected(format!("No such job '{job}'"))),
                },
            };
        };
        let binding = self.store.assistant_binding(&owner_org, &owner_sub)?;
        self.member_owner_or_operator(
            peer_pid,
            binding.as_ref(),
            &owner_org,
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
        peer_pid: u32,
        binding: Option<&crate::store::AssistantBinding>,
        org: &str,
        job: Option<(&str, Option<&str>)>,
    ) -> Result<()> {
        let refuse = || match job {
            Some((_, Some(task))) => Err(Error::rejected(format!("No such task '{task}'"))),
            Some((id, None)) => Err(Error::rejected(format!("No such job '{id}'"))),
            None => Err(Error::rejected("Unknown managed agent")),
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
                refuse()
            }
            caller_rule::Who::Unproven(_) => refuse(),
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

    /// A board relay is not the member-private owner merely because its
    /// connection is operator-proven. A CLI envelope carries the verified
    /// workspace and subject; a plain read_as claim can name only an agent's
    /// own alias, never a human member's private assistant.
    pub(super) fn member_agent_row_withheld_for_relay(
        &self,
        alias: &str,
        params: &Value,
        principal: Option<&super::cli_rpc::CliPrincipal>,
        public: Option<&super::identity::PublicReadPrincipal>,
        peer_pid: u32,
    ) -> Result<bool> {
        if principal.is_none() && public.is_none() && params.get("read_as").is_none() {
            return self.member_agent_row_withheld(alias, peer_pid);
        }
        let Some(binding) = self.store.assistant_for_alias(alias)? else {
            return Ok(false);
        };
        if let Some(reader) = public {
            return Ok(!reader.operator
                && (!binding.is_active()
                    || reader.company != binding.org
                    || reader.subject != binding.sub));
        }
        if let Some(principal) = principal {
            let actor = &principal.actor;
            return Ok(actor.actor != "operator"
                && (actor.principal_kind != "user"
                    || !binding.is_active()
                    || principal.config.company != binding.org
                    || actor.sub != binding.sub));
        }
        if let Some(claim) = params.get("read_as").and_then(Value::as_str) {
            return Ok(claim != "operator"
                && claim.strip_prefix("agents/") != Some(binding.alias.as_str()));
        }
        Ok(true)
    }

    /// Resolve private job exclusions from verified reader identity, never
    /// from a member string asserted by an operator-connected relay.
    pub(super) fn private_job_denials_for_read(
        &self,
        params: &Value,
        cli: Option<&super::cli_rpc::CliPrincipal>,
        public: Option<&super::identity::PublicReadPrincipal>,
        peer_pid: u32,
    ) -> Result<store::PrivateJobDenials> {
        let (mut denied, exclude_cloud) = if let Some(reader) = public {
            if reader.operator {
                (Default::default(), false)
            } else {
                (
                    self.store
                        .private_job_denials(store::PrivateJobReader::User {
                            org: &reader.company,
                            sub: &reader.subject,
                        })?,
                    true,
                )
            }
        } else if let Some(reader) = cli {
            if reader.actor.actor == "operator" {
                (Default::default(), false)
            } else {
                let principal = if reader.actor.principal_kind == "user" {
                    store::PrivateJobReader::User {
                        org: &reader.config.company,
                        sub: &reader.actor.sub,
                    }
                } else {
                    store::PrivateJobReader::Unproven
                };
                (self.store.private_job_denials(principal)?, true)
            }
        } else if params.get("read_as").is_some() {
            (
                self.store
                    .private_job_denials(store::PrivateJobReader::Unproven)?,
                true,
            )
        } else {
            match self.connection_caller(peer_pid)? {
                caller_rule::Who::Operator => (Default::default(), false),
                caller_rule::Who::Agent(alias) => (
                    self.store
                        .private_job_denials(store::PrivateJobReader::Agent(&alias))?,
                    false,
                ),
                caller_rule::Who::Unproven(_) => (
                    self.store
                        .private_job_denials(store::PrivateJobReader::Unproven)?,
                    true,
                ),
            }
        };
        denied.exclude_cloud = exclude_cloud;
        Ok(denied)
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
        // REQ-011: pin the initial avatar look once, from the IMMUTABLE
        // registered alias (never the display name), so a later catalogue
        // addition cannot reroll it. An existing pin is preserved untouched.
        let pinned = self
            .store
            .agent_opt(&binding.alias)?
            .and_then(|agent| agent.params)
            .and_then(|params| {
                params
                    .get("look")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        if pinned.is_none() {
            let look = crate::agent_roles::look_for_alias(&binding.alias);
            self.store
                .set_params(&binding.alias, &json!({ "look": look }))?;
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
    ///
    /// The identity is a bounded read of the verified binding's private
    /// profile files plus the owning agent's pinned avatar look. A read failure
    /// (symlink, oversize, malformed block) propagates: an unreadable profile
    /// is never presented as an unnamed assistant. The private user address and
    /// preferences stay in USER.md and never enter this payload.
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
        let root = self.assistant_principal_cwd(&binding.org, &binding.sub);
        let identity = super::bootstrap_files::read_identity(&root)?;
        let pinned = self
            .store
            .agent_opt(&binding.alias)?
            .and_then(|agent| agent.params)
            .and_then(|params| {
                params
                    .get("look")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        let look = crate::agent_roles::effective_look(&binding.alias, pinned.as_deref());
        let mut assistant = binding.to_json();
        assistant["identity"] = json!({
            "name": identity.name,
            "tone": identity.tone,
            "look": look,
            "revision": binding.bootstrap_revision,
        });
        Ok(json!({
            "assistant": assistant,
            "conversation_id": member.conversation_id,
        }))
    }

    /// Read only the member's bound assistant conversation. Positions are
    /// opaque and member-bound; operator-only rows are excluded before paging.
    pub(super) fn rpc_member_assist_history(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        use cadence_core::board_api::{
            MEMBER_ASSIST_HISTORY_CURSOR_FORMAT, MEMBER_ASSIST_HISTORY_SCOPE_ACK,
            READER_SCOPE_FIELD,
        };

        reject_unknown_fields(
            params,
            &[
                "member_envelope",
                "cursor_format",
                "after",
                "before",
                "tail",
                "limit",
            ],
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
        if params.get("cursor_format").and_then(Value::as_str)
            != Some(MEMBER_ASSIST_HISTORY_CURSOR_FORMAT)
        {
            return Err(Error::invalid(
                "member_assist_history_cursor_format",
                "member assist history requires opaque-v1 cursors",
            ));
        }
        let cursor = |field: &str| -> Result<Option<&str>> {
            match params.get(field) {
                None => Ok(None),
                Some(Value::String(token)) => Ok(Some(token)),
                Some(Value::Number(_)) => Err(Error::invalid(
                    "member_assist_history_cursor_numeric",
                    "member assist history requires an opaque cursor",
                )),
                Some(_) => Err(Error::invalid(
                    "member_assist_history_cursor_invalid",
                    "member assist history cursor is invalid",
                )),
            }
        };
        let after = cursor("after")?;
        let before = cursor("before")?;
        let tail = match params.get("tail") {
            None => false,
            Some(Value::Bool(tail)) => *tail,
            Some(_) => {
                return Err(Error::invalid(
                    "member_assist_history_cursor_invalid",
                    "member assist history direction is invalid",
                ));
            }
        };
        if (after.is_some() && before.is_some()) || (tail && (after.is_some() || before.is_some()))
        {
            return Err(Error::invalid(
                "member_assist_history_cursor_invalid",
                "member assist history takes one paging direction",
            ));
        }
        let conv = member.conversation_id.as_deref().ok_or_else(|| {
            Error::rejected("the member's assistant has no bound conversation yet")
        })?;
        let thread = self
            .store
            .thread_by_id(conv)?
            .filter(|thread| thread.alias == member.alias && !thread.archived)
            .ok_or_else(|| Error::rejected("the bound conversation is not this assistant's"))?;
        let codec = self.scoped_cursor_codec()?;
        let principal = job_events_opaque::member_principal(&member.org, &member.sub);
        let created = format!("{:016x}", thread.created.to_bits());
        let resource = job_events_opaque::framed(&[
            "member-assist-history",
            &member.assistant_id,
            &thread.id,
            &created,
        ]);
        let binding = |direction| CursorBinding {
            domain: "member-assist-history",
            direction,
            principal: &principal,
            resource: &resource,
            filters: "",
        };
        let open = |token: &str, direction| {
            codec.open(token, &binding(direction)).map_err(|_| {
                Error::invalid(
                    "member_assist_history_cursor_invalid",
                    "member assist history cursor is invalid",
                )
            })
        };
        let seal = |position: i64, direction| {
            codec
                .seal(position, &binding(direction))
                .map_err(|_| super::scoped_cursor_config::unavailable())
        };
        let limit = optional_i64(params, "limit")
            .unwrap_or(100)
            .clamp(1, store::THREAD_PAGE_MAX);
        let backwards = tail || before.is_some();
        let (entries, more) = if backwards {
            let position = before
                .map(|token| open(token, CursorDirection::Backward))
                .transpose()?;
            self.store
                .member_thread_entries_before(&thread.id, position, limit)?
        } else {
            let position = after
                .map(|token| open(token, CursorDirection::Forward))
                .transpose()?
                .unwrap_or(0);
            self.store
                .member_thread_entries_after(&thread.id, position, limit)?
        };
        let forward = entries
            .last()
            .map(|entry| seal(entry.seq, CursorDirection::Forward))
            .transpose()?
            .or_else(|| (!backwards).then(|| after.map(str::to_owned)).flatten());
        let backward = entries
            .first()
            .map(|entry| seal(entry.seq, CursorDirection::Backward))
            .transpose()?
            .or_else(|| backwards.then(|| before.map(str::to_owned)).flatten());
        let rows = entries
            .iter()
            .map(|entry| {
                let id = codec
                    .stable_row_id(
                        "member-assist-history-entry",
                        &resource,
                        &entry.seq.to_be_bytes(),
                    )
                    .map_err(|_| super::scoped_cursor_config::unavailable())?;
                let mut row = entry.to_json();
                let fields = row
                    .as_object_mut()
                    .expect("thread entry serializes as an object");
                fields.remove("seq");
                fields.insert("id".to_string(), json!(id));
                Ok(row)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut page = json!({
            "conversation": {"id": thread.id, "alias": thread.alias},
            "entries": rows,
            "cursor": forward,
            "before_cursor": backward,
            "more_before": backwards.then_some(more),
            "more_after": (!backwards).then_some(more),
        });
        page[READER_SCOPE_FIELD] = json!(MEMBER_ASSIST_HISTORY_SCOPE_ACK);
        #[cfg(test)]
        if let Some(fault) = &self.member_history_page_fault {
            match fault.load(std::sync::atomic::Ordering::Acquire) {
                1 => {
                    page.as_object_mut()
                        .expect("page object")
                        .remove(READER_SCOPE_FIELD);
                }
                2 => page["cursor"] = json!(1),
                3 => page[READER_SCOPE_FIELD] = json!("wrong-scope"),
                4 => page["conversation"]["updated"] = json!(123.0),
                _ => {}
            }
        }
        Ok(page)
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

    /// `member_job_request` — durable, idempotent member job request on
    /// `(org, sub, request_key)`.
    ///
    /// Every valid request is ACCEPTED before any job publication: the
    /// `assist_request_queue` row is the single durable record, so an
    /// issue already held by another nonterminal job yields a pending
    /// request (promoted autonomously once the issue is free) rather than
    /// an occupancy-dependent refusal or a second job.
    ///
    /// The immediate receipt is occupancy-independent and identical on a
    /// replay: exactly `{request_id, status:"accepted"}`. It names no job,
    /// no holder and no pending/created state. Owner-scoped detail (the
    /// eventual authorized job id and whether it is pending/created) is
    /// read through `member_job_list.requests`.
    ///
    /// CAD-2215: a request names a tracker `issue` and must resolve —
    /// uniquely — to a project THIS member may read NOW. An unreadable,
    /// ambiguous or withdrawn issue refuses opaquely before any durable
    /// write. The canonical job, once promoted, is `open` with the
    /// member's bound assistant as `pm_alias` (it REQUESTS and follows,
    /// never approves/dispatches) and a draft task carrying the request's
    /// title/acceptance. Nothing auto-dispatches: operator/PM approval
    /// still gates the kickoff. One leaf issue still maps to one
    /// non-terminal job.
    pub(super) fn rpc_member_job_request(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &[
                "member_envelope",
                "title",
                "request_key",
                "issue",
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
            .write()
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
        // CAD-2215: the tracker issue is the request's project scope. It is
        // required, and it must resolve to a project this member may read
        // NOW — before the acceptance row is written. An unreadable,
        // ambiguous or withdrawn issue refuses opaquely with no durable
        // effect.
        let issue = required_str(params, "issue")?;
        if issue.trim().is_empty() {
            return Err(Error::rejected("member job request needs a nonempty issue"));
        }
        let body = optional_str(params, "body").unwrap_or_default();
        let acceptance = optional_str(params, "acceptance").unwrap_or_default();
        // The member's own project scope: readable projects and the
        // issue→project map, read from the tracker + project policy. A
        // member never has the operator's unrestricted (`None`) scope.
        let scope = self.member_project_scope(&member)?;
        self.member_issue_readable(&scope, issue)?;
        // The one occupancy-independent receipt: a fresh request and a
        // replay answer identically, naming neither a job nor the issue's
        // current availability. Owner-scoped detail (`pending`/`created`,
        // the eventual authorized job id) is read through `job_list`.
        let receipt = json!({
            "request_id": store::assist_request_id(&member.org, &member.sub, request_key),
            "status": "accepted",
        });
        // A queued request owns its receipt even if a crash left a bound job
        // before all promotion effects completed. Do not expose or require
        // access to that internal job to replay the acceptance.
        let queued = self
            .store
            .assist_request_queued(&member.org, &member.sub, request_key)?;
        if queued.is_none() {
            // Legacy requests have no queue row. Their existing job still
            // must be readable and bound to the requested issue.
            if let Some(job) =
                self.store
                    .assist_request_job(&member.org, &member.sub, request_key)?
            {
                self.member_job_for_read(&member, &scope, &job)?;
                self.revalidate_member_job_read(params, &member, &scope, "job_request", None)?;
                let existing = self.member_job_for_read(&member, &scope, &job)?;
                if existing.issue_id.as_deref() != Some(issue) {
                    return Err(Error::rejected(
                        "member job request replay refused: this request_key is already \
                         recorded against a different issue",
                    ));
                }
                return Ok(receipt);
            }
        }
        // A queued replay revalidates its recorded issue. Promotion belongs
        // to the joined worker, never to request-time work.
        let _record = match queued {
            Some(record) => {
                if record.issue != issue {
                    return Err(Error::rejected(
                        "member job request replay refused: this request_key is already \
                         recorded against a different issue",
                    ));
                }
                record
            }
            None => {
                // Fresh acceptance: the durable row lands BEFORE any job
                // publication, so an occupied issue cannot turn a valid
                // request into a refusal. The enqueue transaction owns the
                // no-retarget rule for a concurrent second caller.
                self.store.assist_request_enqueue(
                    &member.org,
                    &member.sub,
                    request_key,
                    issue,
                    title,
                    body,
                    acceptance,
                    &member.credential_id,
                )?
            }
        };
        // The durable row already exists (fresh or an earlier acceptance).
        // No promotion is attempted here: the immediate receipt must not
        // depend on hidden occupancy, timing or the tracker. Wake the joined
        // worker so the accepted work is event-driven; this call is the
        // same admission path whether the issue is free or held.
        self.assist_requests_changed.notify_all();
        Ok(receipt)
    }

    /// CAD-2215: attempt to turn one accepted request into its canonical
    /// draft job. `Ok(Some(job))` means the job now exists (created by this
    /// attempt or adopted after a crash); `Ok(None)` means the issue is
    /// still held or the tracker is busy, so the request stays pending.
    ///
    /// Only the joined autonomous worker calls this. The request's ORIGINAL
    /// recorded credential is re-proven live before any publication — never
    /// the member's latest — through the daemon's enrolled `hosted-board`
    /// `assist.revalidate` service bearer. A missing original credential or
    /// a missing service binding fails closed. Current project/issue
    /// authority is re-read under the tracker lock immediately before the
    /// job row is written.
    fn assist_request_promote(
        &self,
        member: &MemberCaller,
        record: &store::AssistRequestQueued,
        scope: &Option<super::identity::ProjectScope>,
    ) -> Result<Option<String>> {
        if record.credential_id.trim().is_empty() {
            return Err(Error::rejected(
                "member job request promotion refused: the request has no recorded \
                 original credential",
            ));
        }
        // Use the id persisted at acceptance — never a fresh derivation —
        // so a replay/crash-adoption targets exactly the queued request's
        // own canonical job and delivery provenance matches before the
        // ledger exists.
        let job_id = record.planned_job_id.clone();
        // Re-prove the request's ORIGINAL credential and current issue
        // binding BEFORE any branch decision, including the occupancy check.
        // An unavailable authority/service therefore reports the same
        // truthful outcome whether the issue is free or held; it can never
        // become a side channel for a hidden occupant. Each effect path then
        // rechecks the current issue scope again immediately before writing.
        self.reprove_member_job_promotion(member, record, scope)?;
        if !self.store.assist_queue_promotion_unambiguous(record)? {
            return Err(Error::rejected(
                "member job request promotion refused: provenance unavailable",
            ));
        }
        // An already-bound ledger is authoritative: never mint a second job.
        // The idempotent recovery (subscription + notice) is still an
        // effect, and the top reproof above covers it.
        if let Some(job) =
            self.store
                .assist_request_job(&member.org, &member.sub, &record.request_key)?
        {
            let Some(row) = self.store.job_opt(&job)? else {
                return Err(Error::rejected(MEMBER_JOB_ISSUE_UNAVAILABLE));
            };
            if row.issue_id.as_deref() != Some(record.issue.as_str()) {
                return Err(Error::rejected(MEMBER_JOB_ISSUE_UNAVAILABLE));
            }
            self.member_issue_readable(&self.member_project_scope(member)?, &record.issue)?;
            self.finish_assist_promotion(member, record, &job)?;
            return Ok(Some(job));
        }
        let spec_path = self.assist_job_spec_path(&job_id);
        // A job row without its ledger is a partial publication from a
        // crashed attempt. Distinguish "absent" from a store failure — a
        // failed read is never treated as an absent job — then adopt only
        // when every immutable field and artifact byte matches this request.
        match self.store.job_opt(&job_id)? {
            Some(existing) => {
                let expected =
                    Self::assist_job_spec_text(&record.title, &record.body, &record.acceptance);
                let artifact_matches = std::fs::read(&spec_path)
                    .ok()
                    .is_some_and(|bytes| bytes == expected.as_bytes());
                if existing.pm_alias != member.alias
                    || existing.issue_id.as_deref() != Some(record.issue.as_str())
                    || existing.spec_path != spec_path.to_string_lossy().as_ref()
                    || !artifact_matches
                {
                    return Err(Error::rejected(MEMBER_JOB_ISSUE_UNAVAILABLE));
                }
                // Adoption binds the ownership ledger and completes the
                // idempotent effects; the top reproof covers the original
                // credential, and the current issue scope is rechecked here
                // immediately before the write.
                self.member_issue_readable(&self.member_project_scope(member)?, &record.issue)?;
                let bound = self.store.assist_request_bind(
                    &member.org,
                    &member.sub,
                    &record.request_key,
                    &existing.id,
                    &record.title,
                    &record.body,
                    &record.acceptance,
                    &record.credential_id,
                )?;
                self.finish_assist_promotion(member, record, &bound)?;
                Ok(Some(bound))
            }
            None => {
                // An ordinary nonterminal job holds the issue: nothing to
                // prove or publish yet. `create_job` re-checks inside its
                // own transaction.
                if self.store.has_open_job_for_issue(&record.issue)? {
                    return Ok(None);
                }
                // The top reproof already proved the original credential;
                // nothing to repeat before the occupancy check.
                // Serialize the final scope proof with supported tracker
                // writes. The caller may already hold `member_job_lock`;
                // never wait for the tracker writer and risk a lock-order
                // deadlock.
                let pm = self.pm()?;
                let Some(_tracker_guard) = pm.try_lock()? else {
                    return Ok(None); // tracker busy: the next sweep retries
                };
                // Re-read the issue binding immediately before publishing. A
                // revoked grant or a withdrawn issue fails closed; nothing is
                // created.
                self.member_issue_readable(&self.member_project_scope(member)?, &record.issue)?;
                if self.store.has_open_job_for_issue(&record.issue)? {
                    return Ok(None);
                }
                let (spec_path, spec_created) =
                    self.assist_job_spec(&job_id, &record.title, &record.body, &record.acceptance)?;
                let spec_sha = match sha256_file(std::path::Path::new(&spec_path)) {
                    Some(sha) => sha,
                    None => {
                        if spec_created {
                            std::fs::remove_file(&spec_path)?;
                        }
                        return Err(Error::internal("assist job spec could not be hashed"));
                    }
                };
                // Create the real job bound to the assistant alias as its PM
                // (the assistant tracks, never auto-dispatches) and to the
                // requested tracker issue. Nothing here approves, promotes or
                // dispatches.
                let created = self.store.create_job(
                    &job_id,
                    Some(&record.title),
                    &spec_path,
                    &spec_sha,
                    &member.alias,
                    Some(&record.issue),
                    None,
                    None,
                    2,
                    None,
                    Some(&record.title),
                    None,
                    None,
                    None,
                    None, // no assignee: a draft task awaits the operator's dispatch
                    if record.acceptance.is_empty() {
                        None
                    } else {
                        Some(record.acceptance.as_str())
                    },
                );
                let (_dup, job) = match created {
                    Ok(created) => created,
                    Err(error) => {
                        if spec_created {
                            // The no-clobber spec belongs to this attempt.
                            std::fs::remove_file(&spec_path)?;
                        }
                        return match error {
                            // A lane that claimed the issue in the race is a
                            // contention outcome, not a request failure: the
                            // accepted request stays pending. Every rejection
                            // is answered without naming a holder.
                            Error::Rejected(_) => Ok(None),
                            other => Err(other),
                        };
                    }
                };
                let bound = self.store.assist_request_bind(
                    &member.org,
                    &member.sub,
                    &record.request_key,
                    &job.id,
                    &record.title,
                    &record.body,
                    &record.acceptance,
                    &record.credential_id,
                )?;
                self.finish_assist_promotion(member, record, &bound)?;
                Ok(Some(bound))
            }
        }
    }

    /// Re-prove the request's ORIGINAL recorded credential (never the
    /// member's latest) and the member's CURRENT issue binding before every
    /// promotion or crash-recovery effect. An empty envelope selects the
    /// daemon's enrolled `hosted-board` `assist.revalidate` service bearer;
    /// a missing service binding fails closed.
    fn reprove_member_job_promotion(
        &self,
        member: &MemberCaller,
        record: &store::AssistRequestQueued,
        scope: &Option<super::identity::ProjectScope>,
    ) -> Result<()> {
        self.member_authority_live(
            "",
            member,
            MemberReprove {
                operation: "job_request",
                scope: "assist.jobs",
                job_id: None,
                op_credential_id: Some(&record.credential_id),
            },
            "member job request promotion",
        )?;
        self.member_issue_readable(scope, &record.issue)
    }

    /// Finish the exact queued request only while its queue and ledger
    /// provenance remain unique. The subscription and notice are idempotent;
    /// a crash before completion can safely retry them.
    fn finish_assist_promotion(
        &self,
        member: &MemberCaller,
        record: &store::AssistRequestQueued,
        job: &str,
    ) -> Result<()> {
        if job != record.planned_job_id || !self.store.assist_queue_promotion_unambiguous(record)? {
            return Err(Error::rejected(
                "member job request promotion refused: provenance unavailable",
            ));
        }
        self.store
            .job_subscribe(job, &member.org, &member.sub, &member.assistant_id)?;
        self.store.assist_request_notice(job)?;
        if !self.store.assist_queue_promotion_unambiguous(record)? {
            return Err(Error::rejected(
                "member job request promotion refused: provenance unavailable",
            ));
        }
        self.store
            .assist_request_complete(&member.org, &member.sub, &record.request_key)
    }

    /// CAD-2215: autonomous promotion sweep — the daemon watch calls this
    /// on its own cadence. Pending accepted requests progress without the
    /// requester being connected: each is re-proven with its ORIGINAL
    /// recorded credential through the daemon's enrolled `hosted-board`
    /// `assist.revalidate` service bearer, its current project/issue
    /// binding is re-read, and it is promoted only while the issue is
    /// free. A revoked member, a withdrawn grant or an inaccessible issue
    /// is refused (no job), never bypassed.
    ///
    /// Returns one outcome per visited request so the watch log and an
    /// independent test can witness each attempt without elapsed-time
    /// assumptions: `created`, `pending` (issue held / tracker busy),
    /// `refused` (authority, binding or issue no longer admits), `failed`
    /// (an unexpected store/effect error).
    pub(crate) fn route_member_job_requests(&self) -> Result<Vec<AssistPromotion>> {
        // Never publish durable job/spec state while the daemon is closing
        // for shutdown or sits in the operator's manual execution fence.
        if self.closing.load(std::sync::atomic::Ordering::SeqCst) || self.execution_mode.fenced() {
            return Ok(Vec::new());
        }
        let pending = self.store.assist_requests_pending(ASSIST_REQUEST_BATCH)?;
        let mut outcomes = Vec::with_capacity(pending.len());
        for record in pending {
            // Bounded and shutdown-responsive: never start another store or
            // external revalidation step once the daemon is closing. An
            // in-flight request finishes within its single bounded pull.
            if self.closing.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let outcome = match self.member_job_lock.write() {
                Ok(_publication) => self.promote_autonomous(&record),
                Err(_) => "failed",
            };
            if outcome != "created" {
                // Persist the truthful reason and rotate this row to the
                // back so a permanently blocked request cannot starve the
                // bound's tail.
                self.store.assist_request_attempted(
                    &record.org,
                    &record.sub,
                    &record.request_key,
                    outcome,
                )?;
            }
            outcomes.push(AssistPromotion {
                request_id: record.request_id.clone(),
                outcome,
            });
        }
        Ok(outcomes)
    }

    /// Test-only driver: invoke the SAME promotion sweep the daemon watch
    /// runs, so a fixture that never starts the watch can still exercise
    /// the real guard and receive the per-request `(request_id, outcome)`
    /// witness. It fabricates no job, skips no re-proof and changes no
    /// authority — it is a call into the production sweep.
    #[cfg(all(test, feature = "test-seam"))]
    pub(crate) fn sweep_member_job_requests_for_test(&self) -> Result<Vec<AssistPromotion>> {
        self.route_member_job_requests()
    }

    /// One pending request's autonomous attempt. A missing/revoked binding
    /// and an inaccessible issue refuse before any effect; the promotion
    /// itself re-proves the recorded original credential.
    fn promote_autonomous(&self, record: &store::AssistRequestQueued) -> &'static str {
        let binding = match self.store.assistant_binding(&record.org, &record.sub) {
            Ok(Some(binding)) if binding.is_active() => binding,
            Ok(_) => return "refused",
            Err(_) => return "failed",
        };
        let member = MemberCaller {
            org: record.org.clone(),
            sub: record.sub.clone(),
            assistant_id: binding.assistant_id.clone(),
            alias: binding.alias.clone(),
            conversation_id: binding.conversation_id.clone(),
            state: binding.state.clone(),
            credential_id: record.credential_id.clone(),
        };
        let scope = match self.member_project_scope(&member) {
            Ok(scope) => scope,
            Err(_) => return "failed",
        };
        if self.member_issue_readable(&scope, &record.issue).is_err() {
            return "refused";
        }
        match self.assist_request_promote(&member, record, &scope) {
            Ok(Some(_)) => "created",
            Ok(None) => "pending",
            Err(Error::Rejected(_)) => "refused",
            Err(_) => "failed",
        }
    }

    /// Write the member's job request to a bounded spec file the job
    /// records — daemon-owned storage, never a caller path. The spec
    /// carries the title/body/acceptance verbatim so the kickoff reads
    /// exactly what the member asked for.
    ///
    /// The text lands through a private temp file and is published without
    /// replacing a prior artifact. `Ok((path, true))` means this call
    /// published it; `Ok((path, false))` means an identical spec from an
    /// earlier crashed attempt is reused — a differing artifact fails
    /// closed and is never overwritten.
    fn assist_job_spec(
        &self,
        job_id: &str,
        title: &str,
        body: &str,
        acceptance: &str,
    ) -> Result<(String, bool)> {
        let path = self.assist_job_spec_path(job_id);
        let dir = path
            .parent()
            .ok_or_else(|| Error::internal("assist job spec path has no directory"))?;
        std::fs::create_dir_all(dir)?;
        let bounded = Self::assist_job_spec_text(title, body, acceptance);
        use std::io::Write;
        let mut temp = tempfile::NamedTempFile::new_in(dir)?;
        if let Err(error) = temp.write_all(bounded.as_bytes()) {
            temp.close()?;
            return Err(error.into());
        }
        match temp.persist_noclobber(&path) {
            Ok(_) => Ok((path.to_string_lossy().to_string(), true)),
            Err(error) => {
                let tempfile::PersistError { error, file } = error;
                file.close()?;
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error.into());
                }
                // A crash after publishing the spec but before the job row
                // leaves this request's own bytes; reuse only an exact
                // match, never overwrite.
                let existing = std::fs::read(&path)?;
                if existing == bounded.as_bytes() {
                    Ok((path.to_string_lossy().to_string(), false))
                } else {
                    Err(Error::rejected(
                        "member job request is partially recorded; no artifact was changed",
                    ))
                }
            }
        }
    }

    /// The exact redacted/bounded spec bytes a request publishes — shared by
    /// publication and by crash-adoption verification, so the two can never
    /// disagree.
    fn assist_job_spec_text(title: &str, body: &str, acceptance: &str) -> String {
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
        bounded
    }

    /// The daemon-owned spec path a canonical job records. Deterministic so
    /// a crashed request can verify and adopt its own artifact.
    fn assist_job_spec_path(&self, job_id: &str) -> std::path::PathBuf {
        self.state_dir
            .join("assist_jobs")
            .join(format!("{job_id}.md"))
    }

    fn member_project_scope(
        &self,
        member: &MemberCaller,
    ) -> Result<Option<super::identity::ProjectScope>> {
        self.project_scope_for_actor(&crate::project_policy::policy_user_actor(&member.sub))
    }

    /// The owner's accepted request rows. Recomputed for the final
    /// reproof so a status advance between projections refuses/retries
    /// instead of mixing snapshots.
    fn member_request_rows(
        &self,
        member: &MemberCaller,
        readable: &std::collections::BTreeSet<String>,
        projects: &std::collections::HashMap<String, String>,
        jobs: &[String],
        worker_running: bool,
    ) -> Result<Vec<Value>> {
        Ok(self
            .store
            .assist_requests_queued(&member.org, &member.sub)?
            .into_iter()
            .filter(|request| {
                projects
                    .get(&request.issue)
                    .is_some_and(|project| readable.contains(project))
            })
            .map(|request| {
                // A bound job is only owner-visible once every required
                // effect (ledger, subscription, inert notice) is durable.
                // Until then the row reads `pending` with no job id, so a
                // crash-interrupted bind can never masquerade as complete.
                // `jobs` is sorted by the scoped store query. A completed
                // flag alone cannot prove that a corrupted queue/ledger
                // binding still names this owner's readable planned job.
                let complete = request.completed
                    && request.job.as_ref().is_some_and(|job| {
                        job == &request.planned_job_id && jobs.binary_search(job).is_ok()
                    });
                let reason = if complete {
                    None
                } else if request.completed {
                    // A completed but unprovable binding cannot be retried by
                    // the worker as a pending request.
                    Some("promotion_attempt_failed")
                } else if !worker_running {
                    // No joined autonomous worker is running: this accepted
                    // request cannot progress and must not read as merely
                    // waiting on a free issue.
                    Some("promotion_worker_unavailable")
                } else {
                    match request.last_outcome.as_deref() {
                        // The sweep could not re-prove the request's original
                        // credential (or the daemon's service bearer is
                        // absent/unauthorized). No occupancy is disclosed.
                        Some("refused") => Some("promotion_unavailable"),
                        Some("failed") => Some("promotion_attempt_failed"),
                        _ => None,
                    }
                };
                json!({
                    "request_id": request.request_id,
                    "issue": request.issue,
                    "status": if complete { "created" } else { "pending" },
                    "job": if complete { request.job } else { None },
                    "reason": reason,
                })
            })
            .collect())
    }

    /// The owner's own private delivery rows, filtered by the current
    /// readable project and re-proved against the unique original assist
    /// request, its job and the job's issue. Subscribers and other members
    /// never match; storage errors fail closed; anything unproved is
    /// omitted.
    fn member_private_deliveries(
        &self,
        member: &MemberCaller,
        readable: &std::collections::BTreeSet<String>,
        projects: &std::collections::HashMap<String, String>,
        jobs: &[String],
    ) -> Result<(Vec<Value>, bool)> {
        let all = crate::delivery::private::load(&self.state_dir)?;
        let mut owner_jobs = Vec::new();
        for job in jobs {
            if self
                .store
                .assist_job_owner(job)?
                .is_some_and(|owner| owner.0 == member.org && owner.1 == member.sub)
            {
                owner_jobs.push(job.as_str());
            }
        }
        let mut unavailable = self.unavailable_private_completions(owner_jobs, &all)?;
        let mut rows = Vec::new();
        for delivery in crate::delivery::private::readable(
            &all,
            crate::delivery::private::Reader::Owner {
                org: &member.org,
                sub: &member.sub,
            },
        ) {
            let Some(project) = projects.get(&delivery.origin.issue) else {
                continue;
            };
            if !readable.contains(project) || project.as_str() != delivery.origin.project {
                continue;
            }
            if unavailable.contains(&delivery.origin.dispatch) {
                continue;
            }
            // Use the same global settled-provenance rule as job reads. An
            // owner-only queue scan misses another principal claiming this
            // job, and must not authorize an ambiguous delivery.
            let Some((owner_org, owner_sub)) = self.store.assist_job_owner(&delivery.origin.job)?
            else {
                continue;
            };
            if owner_org != member.org || owner_sub != member.sub {
                continue;
            }
            let job = self.store.job(&delivery.origin.job)?;
            if job.issue_id.as_deref() != Some(delivery.origin.issue.as_str()) {
                continue;
            }
            if (delivery.state == crate::delivery::private::PrivateState::Completed
                && self.verify_private_delivery_origin(delivery).is_err())
                || (delivery.state == crate::delivery::private::PrivateState::Running
                    && self.verify_private_running_origin(delivery).is_err())
            {
                unavailable.insert(delivery.origin.dispatch.clone());
                continue;
            }
            rows.push(delivery.to_json());
        }
        Ok((rows, unavailable.is_empty()))
    }

    /// The requested tracker issue must resolve — UNIQUELY — to exactly one
    /// project the member may read right now. A missing issue, an id present
    /// in several projects, a project without a member grant and an absent
    /// member project scope all answer the same opaque text: the caller
    /// cannot tell them apart, and no tracker id is echoed back.
    fn member_issue_readable(
        &self,
        scope: &Option<super::identity::ProjectScope>,
        issue: &str,
    ) -> Result<()> {
        let Some((readable, projects)) = scope.as_ref() else {
            return Err(Error::rejected(MEMBER_JOB_ISSUE_UNAVAILABLE));
        };
        match projects.get(issue) {
            Some(project) if readable.contains(project) => Ok(()),
            _ => Err(Error::rejected(MEMBER_JOB_ISSUE_UNAVAILABLE)),
        }
    }

    /// An owning or subscribed member still needs a resolvable, readable
    /// project binding. A withdrawn share does not authorize a subscriber.
    fn member_job_for_read(
        &self,
        member: &MemberCaller,
        scope: &Option<super::identity::ProjectScope>,
        job: &str,
    ) -> Result<store::Job> {
        let unavailable = || Error::rejected("Unknown member job");
        let row = self.store.job(job).map_err(|_| unavailable())?;
        if !super::identity::scope_allows_issue(scope, row.issue_id.as_deref()) {
            return Err(unavailable());
        }
        let Some((owner_org, owner_sub)) = self.store.assist_job_owner(job)? else {
            return Err(unavailable());
        };
        if owner_org != member.org {
            return Err(unavailable());
        }
        let owns = owner_sub == member.sub;
        let subscribed = !owns
            && self
                .store
                .job_is_subscriber(job, &member.org, &member.sub)?
            && self.store.assist_job_is_shared(job, &member.org)?;
        if !owns && !subscribed {
            return Err(unavailable());
        }
        Ok(row)
    }

    fn revalidate_member_job_read(
        &self,
        params: &Value,
        member: &MemberCaller,
        scope: &Option<super::identity::ProjectScope>,
        operation: &str,
        job: Option<&str>,
    ) -> Result<()> {
        self.member_authority_live(
            required_str(params, "member_envelope")?,
            member,
            MemberReprove {
                operation,
                scope: "assist.jobs",
                job_id: job,
                op_credential_id: Some(&member.credential_id),
            },
            "member job read",
        )?;
        if self.member_project_scope(member)? != *scope {
            return Err(Error::rejected(
                "member job read refused: project access changed",
            ));
        }
        if let Some(job) = job {
            self.member_job_for_read(member, scope, job)?;
        }
        Ok(())
    }

    /// `member_job_list` — the jobs this member owns or is subscribed to,
    /// plus this owner's accepted request records. `requests` is additive
    /// and owner-only: each row carries `request_id`, its readable
    /// `issue`, `status` (`pending` while the issue is held,
    /// `created` once the canonical job exists), `job` (`null` until
    /// created) and a truthful pending `reason` (`null` while waiting,
    /// `promotion_unavailable` when the original credential or service
    /// bearer cannot be re-proven, `promotion_attempt_failed` for an
    /// unexpected failure). It is the observation surface — promotion
    /// itself is the joined autonomous worker, never this read.
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
        let scope = self.member_project_scope(&member)?;
        let Some((readable, projects)) = scope.as_ref() else {
            return Err(Error::rejected("member job list requires a project scope"));
        };
        let issues = projects
            .iter()
            .filter(|(_, project)| readable.contains(*project))
            .map(|(issue, _)| issue.as_str())
            .collect::<Vec<_>>();
        let jobs = self
            .store
            .assist_member_jobs_scoped(&member.org, &member.sub, &issues)?;
        // The owner's accepted requests, filtered by the CURRENT readable
        // issue binding. An inaccessible or revoked issue contributes no
        // row and no job detail.
        let worker_running = self
            .assist_request_worker_running
            .load(std::sync::atomic::Ordering::SeqCst);
        let requests =
            self.member_request_rows(&member, readable, projects, &jobs, worker_running)?;
        // CAD-2215: the owner's own private job deliveries. Only the exact
        // verified owner, a current readable project, the unique original
        // assist request and the exact job/issue binding admit a row;
        // subscribers and other members never see this ledger.
        let private_deliveries =
            self.member_private_deliveries(&member, readable, projects, &jobs)?;
        // Test-only observation point: rows projected, no lock held, final
        // authority/project reproof not yet run.
        #[cfg(all(test, feature = "test-seam"))]
        crate::delivery::publication::member_delivery_seam_fire(&self.state_dir);
        // Re-prove the current caller credential, project and ownership
        // after projection: a revocation between the two reads
        // refuses instead of leaking stale results.
        self.revalidate_member_job_read(params, &member, &scope, "job_list", None)?;
        if self
            .store
            .assist_member_jobs_scoped(&member.org, &member.sub, &issues)?
            != jobs
            || self.member_request_rows(&member, readable, projects, &jobs, worker_running)?
                != requests
            || self.member_private_deliveries(&member, readable, projects, &jobs)?
                != private_deliveries
            || self
                .assist_request_worker_running
                .load(std::sync::atomic::Ordering::SeqCst)
                != worker_running
        {
            return Err(Error::rejected("member job list refused: access changed"));
        }
        let mut out = json!({
            "jobs": jobs,
            "requests": requests,
            "private_deliveries": private_deliveries.0,
            "private_deliveries_state": if private_deliveries.1 { "available" } else { "unavailable" },
            "promotion_worker": if worker_running { "running" } else { "unavailable" },
        });
        out[cadence_core::board_api::READER_SCOPE_FIELD] =
            json!(cadence_core::board_api::MEMBER_JOB_SCOPE_ACK);
        // Test-only: observe/mutate the real response after the handler ran.
        #[cfg(all(test, feature = "test-seam"))]
        crate::delivery::publication::member_job_list_seam_fire(&self.state_dir, &mut out);
        Ok(out)
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
        let scope = self.member_project_scope(&member)?;
        let job_row = self.member_job_for_read(&member, &scope, job)?;
        // REAL evidence, revision-bound. A task's state derives only from its
        // server-recorded `head_sha` and the verdict current for its revision;
        // a verdict counts only when it names that exact head. `reported` =
        // no submitted head, or a head with no matching verdict. `unavailable`
        // = the submitted commit is not present in the recorded worktree, even
        // when a matching verdict exists (the verdict stays in `verdict` as
        // audit data only). `inspected` = a revise/blocked verdict on a present
        // artifact; `independently_verified` = a pass on a present artifact.
        // Nothing here fabricates a pass.
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
                "verdict": current.map(|v| super::identity::verdict_json_for_scope(v, true)),
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
        self.revalidate_member_job_read(params, &member, &scope, "job_evidence", Some(job))?;
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
    ///
    /// CAD-2215: a signed member read is ALWAYS scoped — the owner's own
    /// session included, because a member principal is never a proven
    /// operator. Each visible event carries a stable keyed `id` and a
    /// purpose-bound forward `cursor` instead of the global `seq`, and the
    /// page continues from its last visible row's token.
    pub(super) fn rpc_member_job_events(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["member_envelope", "job", "after", "tail", "cursor_format"],
            "member job events",
        )?;
        // The opaque format is the ONLY format this read speaks: a missing,
        // unknown or numeric continuation is refused, never coerced.
        if job_events_opaque::requested_format(params, "member job events")?
            != Some(job_events_opaque::FORMAT)
        {
            return Err(job_events_opaque::format_required("member job events"));
        }
        let supplied = match job_events_opaque::supplied_after(params) {
            job_events_opaque::SuppliedAfter::Absent => None,
            job_events_opaque::SuppliedAfter::Text(token) => Some(token),
            job_events_opaque::SuppliedAfter::Number => {
                return Err(job_events_opaque::numeric_cursor("member job events"))
            }
            job_events_opaque::SuppliedAfter::Unsupported => {
                return Err(job_events_opaque::invalid_after("member job events"))
            }
        };
        let member = self.member_caller(
            params,
            peer_pid,
            "member job events",
            "job_events",
            crate::assistant_actor::AssistScope::Jobs,
            params.get("job").and_then(Value::as_str),
        )?;
        let job = required_str(params, "job")?;
        let scope = self.member_project_scope(&member)?;
        // The member's current project binding, the stored job and the
        // owner/subscriber policy, from the store, before any event is read.
        let row = self.member_job_for_read(&member, &scope, job)?;
        // Missing key material refuses before any content; it never
        // downgrades to numeric positions.
        let codec = self.scoped_cursor_codec()?;
        let principal = job_events_opaque::member_principal(&member.org, &member.sub);
        let position = match supplied {
            Some(token) => Some(job_events_opaque::open(
                &codec,
                &principal,
                &row,
                token,
                "member job events",
            )?),
            None => None,
        };
        let tail = params.get("tail").and_then(Value::as_bool).unwrap_or(false);
        let events = if tail {
            self.store.job_events_tail(job, 51)?
        } else {
            self.store.job_events(job, position.unwrap_or(0), 200)?
        };
        let has_older = tail.then_some(events.len() > 50);
        let events = &events[usize::from(has_older == Some(true))..];
        // Re-prove the live credential, the project scope, the owner or
        // subscriber grant and the exact stored job AFTER selection: a
        // revocation or a job-identity change refuses instead of returning a
        // stale page.
        self.revalidate_member_job_read(params, &member, &scope, "job_events", Some(job))?;
        let current = self.store.job(job)?;
        if current.issue_id != row.issue_id
            || job_events_opaque::job_resource(&current) != job_events_opaque::job_resource(&row)
        {
            return Err(Error::rejected(
                "member job events refused: reader access changed",
            ));
        }
        let mut page = job_events_opaque::scoped_page(
            &codec,
            &principal,
            &row,
            events,
            // A tail page ignores `after`; its cursor is the last visible row
            // or null, never a re-echoed token. A forward page echoes the
            // exact supplied token when it has no rows.
            if tail { None } else { supplied },
            has_older,
            "member job events",
        )?;
        page["job"] = json!(job);
        page[cadence_core::board_api::READER_SCOPE_FIELD] =
            json!(cadence_core::board_api::JOB_EVENTS_SCOPE_ACK);
        Ok(page)
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
        let scope = self.member_project_scope(&member)?;
        self.member_job_for_read(&member, &scope, job)?;
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
                "sharing is off — subscribers cannot read the job until sharing resumes"
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
        let scope = self.member_project_scope(&member)?;
        self.member_job_for_read(&member, &scope, job)?;
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
        // Sharing is not a project grant. The selected job must already
        // resolve to a project this member may read before subscribing.
        let scope = self.member_project_scope(&member)?;
        let record = self
            .store
            .job(job)
            .map_err(|_| Error::rejected("Unknown member job"))?;
        if !super::identity::scope_allows_issue(&scope, record.issue_id.as_deref()) {
            return Err(Error::rejected("Unknown member job"));
        }
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
            "job_record": record.to_json(),
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
        // The same effective identity the authorized UI projection reads: the
        // files are the authority, so an unreadable/malformed profile errors
        // here too rather than briefing the model with a guessed name.
        let identity = super::bootstrap_files::read_identity(&root)?;
        let pinned = self
            .store
            .agent_opt(&binding.alias)?
            .and_then(|agent| agent.params)
            .and_then(|params| {
                params
                    .get("look")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        let look = crate::agent_roles::effective_look(&binding.alias, pinned.as_deref());
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
            "## Effective identity\n\n\
             name (YOUR display name): {name}\n\
             tone (how you communicate): {tone}\n\
             look (your avatar): {look}\n\n\
             `name` is never the user's name. The user's preferred address is \
             `user_name` in USER.md; `preferences` holds other agreed user \
             preferences. Both stay in USER.md and are never your identity.\n\n",
            name = identity.name.as_deref().unwrap_or("(unset)"),
            tone = identity.tone.as_deref().unwrap_or("(unset)"),
        ));
        out.push_str(&format!(
            "## Profile state\n\nstate: {state}\nrevision: {rev}\n\n\
             Your reply already answers the user's request above. Setup is optional.\n\n\
             To record an agreed change on this turn, ACK with a JSON object on stdin, \
             for example: `cadence message ack --bootstrap-file -` with \
             {{\"action\":\"update\",\"expected_revision\":{rev},\"profile\":{{\"name\":\"Ana\"}}}}\n\
             Allowed actions in this state: {allowed}.\n\
             Profile keys: name (your name), tone, user_name (the user's preferred \
             address), preferences, look. An empty string clears a value.\n\
             look must be one of: {looks}.\n\n",
            rev = binding.bootstrap_revision,
            looks = crate::agent_roles::LOOKS.join(", "),
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
