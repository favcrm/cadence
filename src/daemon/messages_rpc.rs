//! CAD-534: `cadence daemon` messages RPC handlers — moved verbatim from src/daemon.rs.

use super::bootstrap_files;
use super::*;

use crate::adapter::InterruptOutcome;
use crate::adapter::Probe;
use crate::issue;
use crate::peer::PeerTies;

/// CAD-565: the largest window one `message read` returns — Unicode
/// scalars, matching the pty paste ceiling so a single page can carry
/// anything a pane could.
pub(super) const MESSAGE_READ_LIMIT: usize = 4_000;

const MESSAGE_LIST_LIMIT_DEFAULT: usize = 20;
const MESSAGE_LIST_LIMIT_MAX: u64 = 50;
const KNOWN_MESSAGE_STATES: &[&str] = &[
    "queued",
    "submitting",
    "running",
    "completed",
    "failed",
    "interrupted",
    "cancelled",
    "unknown",
    "available",
    "refused",
];

fn known_message_state(state: &str) -> bool {
    KNOWN_MESSAGE_STATES.contains(&state)
}

/// Who may list `target`. Stricter than `message_read` for an unproven
/// caller: listing enumerates ids, so an unproven connection is refused
/// even when it could read a body it already named.
pub(super) fn admit_message_list(who: &caller_rule::Who, target: &str) -> Result<()> {
    match who {
        caller_rule::Who::Operator => Ok(()),
        caller_rule::Who::Agent(caller) if caller == target => Ok(()),
        caller_rule::Who::Agent(caller) => Err(Error::rejected(format!(
            "message_list refused: agent '{caller}' cannot list another \
             agent's mail (caller rule)"
        ))),
        caller_rule::Who::Unproven(why) => Err(Error::rejected(format!(
            "message_list refused: an unproven caller cannot list mail ({why})"
        ))),
    }
}

impl Shared {
    /// One retained library image: `{asset_id, revision, sha256}` proved
    /// against the catalog and the stored original digest. No bytes move.
    fn prove_media_refs(&self, value: &Value) -> Result<Value> {
        let rows = value
            .as_array()
            .filter(|rows| rows.len() == 1)
            .ok_or_else(|| Error::rejected("media_refs takes exactly one library image"))?;
        let row = rows[0]
            .as_object()
            .ok_or_else(|| Error::rejected("media_refs entry must be an object"))?;
        if row.len() != 3
            || !row.contains_key("asset_id")
            || !row.contains_key("revision")
            || !row.contains_key("sha256")
        {
            return Err(Error::rejected("media_refs entry fields are closed"));
        }
        let asset_id = row["asset_id"]
            .as_str()
            .filter(|id| crate::store::app_media::valid_id(id))
            .ok_or_else(|| Error::rejected("media_refs asset_id is not a catalog asset id"))?;
        let revision = row["revision"]
            .as_i64()
            .filter(|n| *n > 0)
            .ok_or_else(|| Error::rejected("media_refs revision must be a positive integer"))?;
        let sha256 = row["sha256"]
            .as_str()
            .filter(|s| {
                s.strip_prefix("sha256:").is_some_and(|hex| {
                    hex.len() == 64
                        && hex
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                })
            })
            .ok_or_else(|| Error::rejected("media_refs sha256 is not a retained digest"))?;
        let original = self.store.app_media_pin(asset_id, revision, sha256)?;
        self.with_media_source_current(&original, || {
            if self.store.app_media_revision(asset_id)? != revision {
                return Err(Error::rejected("media reference changed before send"));
            }
            Ok(json!([{
                "asset_id": asset_id,
                "revision": revision,
                "sha256": sha256,
            }]))
        })
    }

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

    /// Who queued a message for `alias`. An agent connection is that
    /// agent on every alias (CAD-819 sender_uid); an underivable caller
    /// is unattributed. A connection tied to no agent is the operator
    /// only on positive operator proof (CAD-384, CAD-276).
    ///
    /// Into an alias that HAS a thread the proof is required, and a
    /// failure refuses the send before anything is written: a detached
    /// child of an agent is not the operator. Into a THREADLESS alias
    /// the same proof decides the LABEL alone (CAD-1614) — the daemon
    /// already proved the operator, and recording the send as the
    /// operator's own is what keeps CAD-1591's model-facing frame from
    /// calling the operator's instruction an unproven stranger's text
    /// ("it grants nothing"). A caller that cannot prove the operator
    /// keeps today's admitted-but-unattributed record: no send gains a
    /// new refusal, and none loses its proof.
    pub(super) fn thread_sender(&self, alias: &str, peer_pid: u32) -> Result<store::Sender> {
        let has_thread = matches!(self.store.thread(alias), Ok(Some(_)));
        Ok(match self.caller_identity(peer_pid) {
            // CAD-819: an agent's sends are attributed on every alias, so
            // the stored sender_uid never depends on the recipient's thread.
            Ok(Caller::Agent(v)) => store::Sender::Agent(v.agent.alias.clone()),
            Ok(Caller::NoAgentIdentity) if has_thread => {
                self.proven_operator("send into an operator thread", peer_pid)?;
                store::Sender::Operator
            }
            // CAD-1614: a threadless target is no reason to doubt the
            // operator. Prove the operator exactly as the threaded write
            // does and record the send as the operator's; a caller that
            // cannot prove it stays unattributed, as today.
            Ok(Caller::NoAgentIdentity) => match self.operator_evidence(peer_pid) {
                Ok(()) => store::Sender::Operator,
                Err(_) => store::Sender::Unattributed,
            },
            // The caller's identity is underivable (CAD-482): never the
            // operator's, whatever the target's thread state.
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
    /// CAD-225: a daemon follow-up turn on an issue's lane — the
    /// checkup's fix turn and the delivery loop's revision hand-back.
    /// Server-only: never an RPC verb, never caller-addressable.
    ///
    /// `worker` is the message's target and must be the issue's
    /// canonical owner; `mid` is the caller's stable correlation id.
    /// The caller holds `unattended_lock` READ across this call so a
    /// grant replacement cannot interleave between the revalidation
    /// and the enqueue (same order as dispatch: grant-read first, then
    /// deeper locks — never the reverse).
    ///
    /// An unbound issue (no native unattended run) takes the ordinary
    /// `send_as` path unchanged. A bound issue must still authorize:
    /// `unattended_dispatch_authority` revalidates the live grant and its
    /// unique registered checkout — a revoked, expired, malformed or
    /// inconsistent grant is an `Err`, never a quiet fall-back to a
    /// plain send. The follow-up then carries the lane's `issue`/
    /// `worktree` tags re-derived from the lane's latest dispatch
    /// queue tag (`worker_issue_lane`), after proving that worktree
    /// still resolves inside the grant's registered repository root.
    /// `worker_issue_turn` correlates the tagged `source='review'` row
    /// (unattributed sender → `sender_uid` NULL) as the lane's latest
    /// owned turn — no second correlation path, no caller-supplied
    /// tags (the CAD-378 guard still refuses `issue`/`worktree` request
    /// fields; the typed `bound_lane` also carries the live grant deadline).
    /// Caller holds grant READ then delivery lock and supplies its current
    /// record. True means a bound follow-up; false preserves legacy sends.
    pub(super) fn send_issue_followup(
        self: &Arc<Self>,
        rec: &crate::delivery::Record,
        mid: &str,
        text: &str,
    ) -> Result<bool> {
        let issue = rec.issue.as_str();
        let worker = rec.worker.as_str();
        let authority = self.unattended_dispatch_authority(issue)?;
        let sender_of = &|_: &str| Ok(store::Sender::Unattributed);
        let steer_refused = &|verb: &str| {
            Err(Error::rejected(format!(
                "{verb} refused: this send carries no caller to steer with"
            )))
        };
        let Some(authority) = authority else {
            // Unbound legacy lane — the pre-grant behavior, unchanged.
            self.send_with(
                &json!({"alias": worker, "text": text, "message": mid, "source": "review"}),
                sender_of,
                steer_refused,
            )?;
            return Ok(false);
        };
        let root = authority.checkout;
        // Bound run: the canonical owner is the only worker a follow-up
        // may target — a reassigned lane ends the run's authority to
        // enqueue, it never re-aims it.
        let pm = self.pm()?;
        let (_project, dir) = issue::write::issue_dir(&pm, issue)?;
        let (front, _) = issue::write::load_front(&dir)?;
        if front.owner.as_deref() != Some(worker) {
            return Err(Error::rejected(format!(
                "{issue}'s unattended run targets its authorized worker — \
                 a follow-up to {worker} is refused"
            )));
        }
        // The lane tag comes from the lane's latest dispatch queue
        // tag — the native record of where this run currently executes.
        // A bound issue with no dispatch kickoff never got its turn, so
        // there is no lane to follow up on.
        let lane = self
            .store
            .worker_issue_lane(issue, worker)?
            .ok_or_else(|| {
                Error::rejected(format!(
                    "{issue}'s unattended run has no native dispatch turn on \
                     {worker} — a follow-up cannot derive its lane"
                ))
            })?;
        // The recorded lane must still resolve inside the grant's
        // registered repository checkout — the same proof
        // `dispatch_send` re-ran on the kickoff itself.
        let actual = issue::project::repo_identity(Path::new(&lane)).ok_or_else(|| {
            Error::rejected(format!(
                "{issue}'s recorded lane has no proven repository identity"
            ))
        })?;
        if actual.0.as_path() != Path::new(&root) {
            return Err(Error::rejected(format!(
                "{issue}'s recorded lane resolves outside its grant's \
                 authorized repository — the follow-up is refused"
            )));
        }
        let head = rec
            .head
            .as_deref()
            .filter(|head| !head.is_empty())
            .ok_or_else(|| Error::rejected("a bound follow-up needs its current recorded head"))?;
        if let Some(intent) = self.store.unattended_merge_intent(issue, head)? {
            let (repo, number) =
                crate::issue::task_report::parse_pr_url(rec.pr.as_deref().unwrap_or_default())?;
            if !intent["repo"]
                .as_str()
                .is_some_and(|bound| bound.eq_ignore_ascii_case(&repo))
                || intent["pr"].as_u64() != Some(number)
                || intent["worker"].as_str() != Some(worker)
            {
                return Err(Error::rejected("the recorded merge intent does not bind this follow-up; operator inspection is required"));
            }
            let base_ref = intent["base_ref"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| Error::rejected("the recorded merge intent has no target branch; operator inspection is required"))?;
            let view = crate::delivery::pr_view(&self.delivery_gh.to_string_lossy(), &repo, number)
                .map_err(|error| Error::rejected(format!("merge-request state is unreadable ({error}); operator must inspect/remove it before repair")))?;
            if view["headRefOid"].as_str() != Some(head)
                || view["state"] != "OPEN"
                || view["baseRefName"] != base_ref
            {
                return Err(Error::rejected(
                    "the PR no longer names this open head; operator must inspect it before repair",
                ));
            }
            // A removed request may have an advanced base, never a different destination.
            let base = view["baseRefOid"]
                .as_str()
                .filter(|oid| !oid.is_empty())
                .ok_or_else(|| {
                    Error::rejected("the current PR has no base commit; repair is refused")
                })?;
            let auto = view.get("autoMergeRequest").ok_or_else(|| {
                Error::rejected(
                    "merge-request evidence is missing; operator must inspect it before repair",
                )
            })?;
            if !auto.is_null() {
                return Err(Error::rejected("the PR still has an armed auto-merge request; operator must remove it before a new head is produced"));
            }
            let entry = super::delivery_requirements_rpc::unattended_queue_entry(
                self, &repo, number, head, base, base_ref,
            ).map_err(|error| Error::rejected(format!("queue-removal proof is unreadable ({error}); operator must inspect/remove it before repair")))?;
            if entry.is_some() {
                return Err(Error::rejected("the PR still has a native queue entry; operator must remove it before a new head is produced"));
            }
        } else if rec.state == crate::delivery::State::Enqueued
            || rec.unattended_pending
            || rec.disable_auto
            || rec.observed.as_ref().is_some_and(|view| view.auto_merge)
        {
            return Err(Error::rejected("the lane has merge-request state without a native intent; operator must inspect/remove it before repair"));
        }
        // Re-prove authority after network reads; carry that expiry into enqueue.
        // Same global writer lock as supported tracker edits; unrelated
        // report files remain outside this read-only snapshot's admission scope.
        let _tracker = pm.lock_for_paths(&[dir.join("issue.md")])?;
        let authorized_until = match self.unattended_dispatch_authority(issue)? {
            Some(reproved) if reproved.checkout == root => reproved.expires_at,
            _ => {
                return Err(Error::rejected(
                    "the bound repository authority changed before follow-up enqueue",
                ));
            }
        };
        const COMPLETION: &str = " End your returned result (or explicit message result text) with a final `SHA: <same full committed head as the done report>` line.";
        let mut body = String::with_capacity(text.len() + COMPLETION.len());
        body.push_str(text);
        body.push_str(COMPLETION);
        let mut params = json!({"alias": worker, "message": mid, "source": "review"});
        params["text"] = Value::String(body);
        self.send_with_human_expected(
            &params,
            sender_of,
            steer_refused,
            None,
            None,
            Some((issue, lane.as_str(), authorized_until)),
        )?;
        Ok(true)
    }

    /// `send_as` with the caller a steering send (CAD-158) is authorized
    /// against, derived from the connection by `steer_caller`.
    pub(super) fn send_with(
        self: &Arc<Self>,
        params: &Value,
        sender_of: &dyn Fn(&str) -> Result<store::Sender>,
        steer_caller: &dyn Fn(&str) -> Result<AgentCaller>,
    ) -> Result<Value> {
        self.send_with_human_expected(params, sender_of, steer_caller, None, None, None)
    }

    /// CAD-1412: `send_with` with an optional expected recipient uid. The
    /// same guards run for every caller (confinement, PTY, control
    /// characters, delivery, steering); the uid is checked only by the
    /// enqueue transaction. `None` is the ordinary path, unchanged.
    pub(super) fn send_with_expected(
        self: &Arc<Self>,
        params: &Value,
        sender_of: &dyn Fn(&str) -> Result<store::Sender>,
        steer_caller: &dyn Fn(&str) -> Result<AgentCaller>,
        expected_uid: Option<&str>,
    ) -> Result<Value> {
        self.send_with_human_expected(params, sender_of, steer_caller, None, expected_uid, None)
    }

    /// The one send body (CAD-1374 + CAD-1412 + CAD-1591): the optional
    /// verified human (typed, daemon-derived) and the optional expected
    /// recipient uid are independent derivations — neither is a request
    /// field, and the recipient uid is never treated as the human
    /// performer. `thread_send` passes both; every other caller reaches
    /// this through a wrapper above with `None`/`None` for whichever it
    /// does not derive.
    /// CAD-225: `bound_lane` is `(issue, worktree, authorized_until)`,
    /// derived server-side by `send_issue_followup`. Other paths pass
    /// `None`; request fields cannot set lane tags or the grant deadline.
    pub(super) fn send_with_human_expected(
        self: &Arc<Self>,
        params: &Value,
        sender_of: &dyn Fn(&str) -> Result<store::Sender>,
        steer_caller: &dyn Fn(&str) -> Result<AgentCaller>,
        human: Option<&Value>,
        expected_uid: Option<&str>,
        bound_lane: Option<(&str, &str, f64)>,
    ) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let text = required_str(params, "text")?;
        let target = self.store.agent_opt(&alias)?;
        if let Some(target) = &target {
            crate::adapter::pi::refuse_confined_devin_agent(target)?;
        }
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
                ));
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
        // with a forged lane (CAD-378 R6). Now only daemon-derived paths
        // write the tags: `dispatch_send` sets them from the lane the
        // daemon itself resolves, `task_dispatch` from the job's rows,
        // `send_issue_followup` from the lane's latest dispatch tag
        // through the typed `bound_lane` argument. A caller field is
        // refused outright — silently dropping it would let a retry
        // pass as untagged and mis-suppress the reported-kickoff
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
        // CAD-565: `turn_id` is the daemon's running-turn token — a
        // nudge binds to the turn that is running when the daemon
        // claims it, checked atomically; a caller field could only ever
        // be a forged reference, so it is refused whole rather than
        // silently ignored.
        if params.get("turn_id").is_some() {
            return Err(Error::rejected(
                "send refused: `turn_id` is not a request field — the daemon \
                 binds turns at delivery; forged turn references are refused",
            ));
        }
        // CAD-1591: `expected_recipient_uid` is a typed derivation —
        // `thread_send` parses, validates and strips it before reaching
        // this body, so a request still carrying it can only be a
        // caller-supplied field on another verb. It is refused whole
        // rather than silently ignored: an agent could otherwise name
        // the constraint and believe a fence ran that never did.
        if params.get("expected_recipient_uid").is_some() {
            return Err(Error::rejected(
                "send refused: `expected_recipient_uid` is not a request \
                 field — the recipient constraint is a daemon-typed \
                 derivation of `thread_send` alone",
            ));
        }
        // A pty endpoint pastes literally and fails a body with control
        // characters at delivery; refuse it here so `send` never answers
        // `queued` for a message that cannot be delivered (CAD-218).
        let pty = target.as_ref().is_some_and(|a| a.endpoint_kind == "pty");
        let mut native_adapter = None;
        if nudge {
            for field in [
                "sender",
                "from",
                "sender_alias",
                "session_id",
                "thread_id",
                "generation",
                "expectedTurnId",
                "expected_turn_id",
                "endpoint",
            ] {
                if params.get(field).is_some() {
                    return Err(Error::rejected(format!(
                        "send --nudge: '{field}' is not accepted; sender and runtime binding are daemon-derived"
                    )));
                }
            }
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
                let supported = registry::spec_opt(&agent.provider, &agent.endpoint_kind)
                    .is_some_and(|spec| spec.supports_native_turn_steering());
                if !supported {
                    return Err(Error::rejected(format!(
                        "--nudge requires a pty or a verified native exact-turn guard — '{alias}' is {}/{}; send a normal queued message instead",
                        agent.provider, agent.endpoint_kind
                    )));
                }
                let adapter = self.adapter_for(&alias)?;
                if !adapter.native_turn_steering() {
                    return Err(Error::rejected(
                        "this live runtime has no verified native exact-turn input guard",
                    ));
                }
                if text.trim().is_empty() {
                    return Err(Error::rejected("native guidance must not be blank"));
                }
                native_adapter = Some(adapter);
            }
            if optional_str(params, "reply_to").is_some() {
                return Err(Error::rejected(
                    "--nudge owes no report, so it takes no reply_to",
                ));
            }
            // N1: a nudge is for an endpoint that exists now — never queued
            // for a stopped or fenced agent to receive later. The endpoint is
            // a pty pane or a verified native adapter.
            let live = self.lifecycle.lock().unwrap().agents.contains_key(&alias)
                && target.as_ref().is_some_and(|a| {
                    (a.endpoint.is_some() || native_adapter.is_some())
                        && matches!(a.state.as_str(), "idle" | "busy")
                });
            if !live {
                return Err(Error::rejected(format!(
                    "agent {alias} has no live endpoint (pty pane or verified native guard)"
                )));
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
        // CAD-802: `thread_send`'s App binding arrives already
        // server-verified (threads_rpc proved it against the store);
        // re-prove here so `agent_send`/`send` can never carry one.
        let app = match params.get("app") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let normalized = thread_app(v, &self.store)?;
                // An installation-only binding is proven by the catalog.
                if normalized.get("context_id").is_none() {
                    self.known_install(normalized["install_id"].as_str().unwrap_or_default())?;
                }
                Some(normalized)
            }
        };
        // CAD-1098: `conversation` only selects among the conversations
        // of the verified App binding's installation. It is checked
        // here (exists, unarchived, same installation, subject still
        // verifies) and again inside the enqueue transaction; it never
        // decides scope, subject or installation.
        let conversation = match params.get("conversation") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) => {
                proto::identifier(id, "conversation ID")?;
                Some(id.as_str())
            }
            Some(_) => return Err(Error::rejected("conversation must be a string id")),
        };
        let (app, home_conversation) = match (app, conversation) {
            (Some(mut app), Some(id)) => {
                self.verify_conversation_selector(&alias, &app, id)?;
                app["conversation"] = json!(id);
                (Some(app), None)
            }
            (None, Some(id)) => {
                if !crate::master::is_master(&alias) {
                    return Err(Error::rejected(
                        "only the Master can select a Home conversation",
                    ));
                }
                self.prove_home_conversation(Some(id))?;
                (None, Some(id.to_string()))
            }
            (app, None) => (app, None),
        };
        // CAD-1168: `thread_send`'s `attachments` — retained-file ids
        // resolved to their stored metadata rows (existence + size
        // re-checked server-side, scope bound to the verified app
        // binding above). The field is `thread_send`'s alone, like
        // `refs`/`app`: a `send`/`ask` carrying it is refused whole,
        // before any file is looked at, never silently stripped.
        let sender = sender_of(&alias)?;
        if refs.is_some() && sender != store::Sender::OperatorChat {
            return Err(Error::rejected(
                "refs is a thread_send field — only the operator's chat cites \
                 needs rows; `cadence send` and `agent_send` carry none",
            ));
        }
        let attachments = match params.get("attachments") {
            None | Some(Value::Null) => None,
            Some(_) if sender != store::Sender::OperatorChat => {
                return Err(Error::rejected(
                    "attachments is a thread_send field — only the operator's chat \
                     attaches retained files; `cadence send` and `agent_send` carry none",
                ));
            }
            Some(_) if !crate::master::is_master(&alias) => {
                return Err(Error::rejected(
                    "attachments go only to the master (home or app conversation turns) — \
                     other agents cannot read retained chat files",
                ));
            }
            Some(v) => {
                let account_id = params
                    .get("session")
                    .map(|proof| self.chat_upload_session_account(proof))
                    .transpose()?
                    .flatten();
                let home_thread = if app.is_none() {
                    Some(match home_conversation.as_deref() {
                        Some(selected) => selected.to_string(),
                        None => self.prove_home_conversation(None)?,
                    })
                } else {
                    None
                };
                Some(thread_attachments(
                    self,
                    v,
                    app.as_ref(),
                    account_id.as_deref(),
                    params.get("session").is_some(),
                    home_thread.as_deref().unwrap_or_default(),
                )?)
            }
        };
        let media_refs = match params.get("media_refs") {
            None | Some(Value::Null) => None,
            Some(_) if sender != store::Sender::OperatorChat => {
                return Err(Error::rejected(
                    "media_refs is a thread_send field — only the operator's chat pins a library image",
                ));
            }
            Some(value) => Some(self.prove_media_refs(value)?),
        };
        // CAD-1098: app conversations are the master's. Another agent has
        // no per-conversation session, so an app binding to it is refused.
        if home_conversation.is_some() && sender != store::Sender::OperatorChat {
            return Err(Error::rejected(
                "Home conversation is a thread_send-only selector",
            ));
        }
        if app.is_some() && !crate::master::is_master(&alias) {
            return Err(Error::rejected(
                "an app binding is only for the master's chat — other agents \
                 have no per-app conversations",
            ));
        }
        if app.is_some() && sender != store::Sender::OperatorChat {
            return Err(Error::rejected(
                "app is a thread_send field — only the operator's chat carries \
                 a verified App binding; `cadence send` and `agent_send` carry none",
            ));
        }
        if let Some(adapter) = native_adapter {
            let target = target
                .as_ref()
                .ok_or_else(|| Error::rejected("Unknown managed agent"))?;
            // Direct submission through the concurrent-control seam: the actor
            // is still waiting for its original kickoff's provider result.
            // The store atomically binds/claims this turnless row, so it cannot
            // be replayed by the normal actor queue or a concurrent duplicate.
            let (duplicate, _, bound) = self
                .store
                .begin_native_nudge(target, text, &message, &sender, &steer)?;
            if !duplicate {
                if let Some(turn) = bound.as_deref() {
                    let (disposition, reason) = match adapter.steer_turn(turn, text, &message) {
                        Ok(crate::adapter::SteerOutcome::Queued) => ("queued", None),
                        Ok(crate::adapter::SteerOutcome::NotRunning) => ("skipped_inactive", None),
                        Err(
                            Error::Rejected(reason)
                            | Error::Provider(reason)
                            | Error::PreWrite(reason),
                        ) => ("rejected", Some(reason)),
                        // Without a definitive refusal, acceptance may have
                        // happened. Preserve uncertainty, never requeue.
                        Err(error) => ("unknown", Some(error.to_string())),
                    };
                    self.store
                        .finish_native_nudge(&message, disposition, reason.as_deref())?;
                }
            }
            let stored = self
                .store
                .message(&message)?
                .ok_or_else(|| Error::internal("Native steering row vanished"))?;
            self.wake();
            return Ok(
                json!({"message": message, "state": stored.state, "duplicate": duplicate,
                "turn_id": stored.turn_id, "delivery": "native_turn_steering", "disposition": stored.result}),
            );
        }
        // CAD-1586: a fresh send to a master whose stored launch
        // configuration provably refuses every launch is refused
        // `master_unlaunchable` here — after every request check,
        // before the durable enqueue — so no route can queue a turn no
        // launch can deliver. Busy, starting, stopping and
        // offline-but-launchable masters are unaffected.
        // A retry of an ID that already names a row is decided by the
        // store's exact-envelope comparison; the refusal is for new sends.
        if crate::master::is_master(&alias) && !self.store.message_exists(&message)? {
            self.master_send_preflight(&alias)?;
        }
        let (duplicate, state) = self.store.enqueue_steered_for_with_conversation(
            &alias,
            expected_uid,
            text,
            reply_to.as_deref(),
            &message,
            source,
            task,
            bound_lane.map(|(issue, _, _)| issue),
            bound_lane.map(|(_, worktree, _)| worktree),
            &sender,
            &steer,
            refs.as_ref(),
            app.as_ref(),
            attachments.as_ref(),
            home_conversation.as_deref(),
            human,
            media_refs.as_ref(),
            bound_lane.map(|(_, _, until)| until),
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

    /// `message read` — the pull half of CAD-565's push/pull delivery:
    /// the stored body in bounded windows of Unicode scalars, never
    /// bytes. An agent caller reads only its own mail (the caller
    /// rule); the operator and unattributed readers pass, exactly as
    /// on the inbox drain (CAD-480).
    pub(super) fn rpc_message_read(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_identity_fields(params, "message_read")?;
        let id = required_str(params, "message")?;
        let message = self
            .store
            .message(id)?
            .ok_or_else(|| Error::rejected(format!("No such message '{id}'")))?;
        if message.source == "app_run_dispatch" {
            match self.connection_caller(peer_pid)? {
                caller_rule::Who::Operator => {}
                _ => {
                    return Err(Error::rejected(
                        "app mail inspection requires operator proof",
                    ));
                }
            }
        }
        if let caller_rule::Who::Agent(caller) = self.connection_caller(peer_pid)? {
            if !self.store.agent_reads_message(&message, &caller)? {
                return Err(Error::rejected(format!(
                    "message_read refused: agent '{caller}' cannot read another \
                     agent's mail (caller rule, CAD-480)"
                )));
            }
        }
        // CAD-1380 target-side privacy: a member assistant's stored message is
        // readable only by the operator or that assistant itself. An unproven
        // caller is refused, never served the body.
        if self.store.assistant_for_alias(&message.alias)?.is_some() {
            match self.connection_caller(peer_pid)? {
                caller_rule::Who::Operator => {}
                caller_rule::Who::Agent(caller) if caller == message.alias => {}
                _ => {
                    return Err(Error::rejected(
                        "message_read refused: a member assistant's message is private \
                         to its assistant (CAD-1380 target-side privacy)",
                    ))
                }
            }
        }
        let offset = optional_u64(params, "offset").unwrap_or(0) as usize;
        let limit = optional_u64(params, "limit")
            .unwrap_or(MESSAGE_READ_LIMIT as u64)
            .min(MESSAGE_READ_LIMIT as u64) as usize;
        let total = message.body.chars().count();
        let text: String = message.body.chars().skip(offset).take(limit).collect();
        // CAD-802: the verified App hint as receipt metadata — the
        // pulled text stays exactly the stored body (offsets stable).
        // A hint that cannot be re-proved is null, never an error.
        let app_context = self
            .store
            .message_app(&message.id)
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        Ok(json!({
            "message": message.id,
            "text": text,
            "offset": offset,
            "limit": limit,
            "chars_total": total,
            "truncated": offset + text.chars().count() < total,
            "app_context": app_context,
        }))
    }

    /// `message list` — ids and state for one recipient. No body, result,
    /// error, or turn token. An agent lists only its own alias. An
    /// unproven caller is refused. App-owned rows are included only for
    /// an operator, and still without their body.
    pub(super) fn rpc_message_list(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_identity_fields(params, "message_list")?;
        let to = required_str(params, "to")?;
        let alias = self.resolve_alias(to)?;
        let who = self.connection_caller(peer_pid)?;
        admit_message_list(&who, &alias)?;
        let states = match params.get("state") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .filter(|state| known_message_state(state))
                        .map(str::to_string)
                        .ok_or_else(|| {
                            Error::rejected(format!(
                                "state must be one of {}",
                                KNOWN_MESSAGE_STATES.join(", ")
                            ))
                        })
                })
                .collect::<Result<Vec<_>>>()?,
            Some(_) => return Err(Error::rejected("state must be an array of message states")),
        };
        let from = optional_str(params, "from");
        let source = optional_str(params, "source");
        if from.is_some_and(|value| value.is_empty())
            || source.is_some_and(|value| value.is_empty())
        {
            return Err(Error::rejected(
                "from and source, when set, must be nonempty",
            ));
        }
        let since = optional_str(params, "since").map(str::to_string);
        let limit = match optional_u64(params, "limit") {
            None => MESSAGE_LIST_LIMIT_DEFAULT,
            Some(n) if (1..=MESSAGE_LIST_LIMIT_MAX).contains(&n) => n as usize,
            Some(_) => {
                return Err(Error::rejected(format!(
                    "limit must be 1 to {MESSAGE_LIST_LIMIT_MAX}"
                )))
            }
        };
        let include_app = matches!(who, caller_rule::Who::Operator);
        let page = self.store.message_index(
            &alias,
            &store::MessageIndexQuery {
                states: &states,
                from,
                source,
                since: since.as_deref(),
                limit,
                include_app,
            },
        )?;
        Ok(json!({
            "alias": alias,
            "messages": page.rows.iter().map(store::MessageIndex::to_json).collect::<Vec<_>>(),
            "limit": limit,
            "omitted": page.omitted,
            "truncated": page.omitted > 0,
        }))
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
        // CAD-565: the recovery re-sends the same delivery the pane got
        // — the bounded notice, not the full body (endpoint_kind is
        // already proven `pty` above).
        let delivery = self.delivery_body(&alias, "pty", &message, None);
        let outcome = adapter.recover_submit(&generation, &delivery, &confirm);
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
                );
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

    /// CAD-880: the (message id, submission token) a report acts on (I1,
    /// I4). Both fields present: the explicit path, with git-style prefix
    /// resolution on the id. Both absent: the caller's single held turn,
    /// resolved from the connection caller. Exactly one of the two is a
    /// refusal — a token without its message names nothing, and a message
    /// without its token proves nothing.
    pub(super) fn report_target(&self, params: &Value, peer_pid: u32) -> Result<(String, String)> {
        let id = optional_str(params, "message");
        let token = optional_str(params, "token");
        match (id, token) {
            (Some(id), Some(token)) => Ok((
                self.resolve_message_id_for(peer_pid, id)?,
                token.to_string(),
            )),
            (None, None) => self.caller_turn(peer_pid),
            (Some(_), None) => Err(Error::rejected(
                "message report needs both the message id and its --token, or neither \
                 (neither reports your own single running turn)",
            )),
            (None, Some(_)) => Err(Error::rejected(
                "message report needs both the message id and its --token, or neither \
                 (a token without its message names nothing)",
            )),
        }
    }

    /// CAD-880 (F1): explicit-id resolution. An exact id resolves globally
    /// — bearer possession, unchanged for every caller. A prefix
    /// enumerates only the caller's own alias for agent callers (an
    /// ambiguity refusal names only their own ids), globally for the
    /// operator, and not at all for unproven callers (exact id only —
    /// possession still works, prefix fishing does not). Resolution never
    /// bypasses the token/generation/state gates below (I4).
    pub(super) fn resolve_message_id_for(&self, peer_pid: u32, id: &str) -> Result<String> {
        if self.store.message(id)?.is_some() {
            return Ok(id.to_string());
        }
        if id.len() < 8 {
            return Err(Error::rejected(format!(
                "No such message '{id}' — pass the full id or a unique prefix of \
                 at least 8 chars"
            )));
        }
        let matches = match self.connection_caller(peer_pid)? {
            caller_rule::Who::Agent(alias) => self.store.message_id_prefix_for(id, &alias)?,
            caller_rule::Who::Operator => self.store.message_id_prefix(id)?,
            caller_rule::Who::Unproven(_) => {
                return Err(Error::rejected(format!("No such message '{id}'")));
            }
        };
        Self::pick_prefix_match(id, matches)
    }

    /// CAD-880: one candidate from a prefix pool — unique wins, none or
    /// several are refused, the latter listing the matches.
    fn pick_prefix_match(id: &str, mut matches: Vec<String>) -> Result<String> {
        matches.retain(|m| m != id);
        if matches.is_empty() {
            return Err(Error::rejected(format!("No such message '{id}'")));
        }
        if matches.len() > 1 {
            matches.sort();
            let shown = matches
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let count = if matches.len() > 10 {
                "more than 10".to_string()
            } else {
                matches.len().to_string()
            };
            return Err(Error::rejected(format!(
                "Ambiguous message prefix '{id}' ({count} matches): {shown}"
            )));
        }
        Ok(matches.into_iter().next().unwrap())
    }

    /// CAD-880: the caller's single held turn (I1, I2) — the daemon-side
    /// counterpart of `cadence self`. The caller comes ONLY from
    /// `connection_caller`: a peer resolves only its own turns, the
    /// operator (which runs no turn) and an unproven or detached caller
    /// are refused before any write.
    fn caller_turn(&self, peer_pid: u32) -> Result<(String, String)> {
        let alias = match self.connection_caller(peer_pid)? {
            caller_rule::Who::Agent(alias) => alias,
            caller_rule::Who::Operator => {
                return Err(Error::rejected(
                    "message report with no id reports the caller's own running turn — \
                     this connection is provably the operator, which runs no turn; \
                     pass an explicit message id and token",
                ));
            }
            caller_rule::Who::Unproven(why) => {
                return Err(Error::rejected(format!(
                    "message report with no id reports the caller's own running turn — \
                     this connection derives no agent identity and is not provably \
                     the operator: {why}"
                )));
            }
        };
        let held = self.store.held_turns(&alias)?;
        match held.len() {
            0 => Err(Error::rejected(format!(
                "agent '{alias}' has no running turn — pass an explicit message id \
                 and token"
            ))),
            1 => {
                let m = held.into_iter().next().unwrap();
                let token = m.turn_id.clone().ok_or_else(|| {
                    Error::rejected(format!(
                        "message {} holds a turn with no submission token — report it \
                         explicitly",
                        m.id
                    ))
                })?;
                Ok((m.id, token))
            }
            _ => {
                let ids = held
                    .iter()
                    .map(|m| m.id.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(Error::rejected(format!(
                    "agent '{alias}' holds {} running turns — report one explicitly: {ids}",
                    held.len()
                )))
            }
        }
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
    pub(super) fn rpc_message_report(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        // CAD-880 (I3): caller identity is connection-bound — an `alias`
        // field would name whose turn a default resolves, so it is
        // refused outright rather than ignored.
        if params.get("alias").is_some() {
            return Err(Error::rejected(
                "message report: caller identity is connection-bound; request field \
                 'alias' is not accepted",
            ));
        }
        reject_identity_fields(params, "message report")?;
        let kind = required_str(params, "kind")?;
        let text = optional_str(params, "text");
        // CAD-1420: a bootstrap object is an ACK-only field; the caller is
        // derived from the connection, never from the request.
        if params.get("bootstrap").is_some() && kind != "ack" {
            return Err(Error::rejected(
                "bootstrap is accepted only on a message ack",
            ));
        }
        let (id, token) = self.report_target(params, peer_pid)?;
        let mut message = self
            .store
            .message(&id)?
            .ok_or_else(|| Error::rejected("Unknown message"))?;
        if message.source == "app_run_dispatch" {
            let caller = self.agent_caller(peer_pid, "app material report")?;
            if !matches!(caller, AgentCaller::Agent(ref alias) if alias == &message.alias) {
                return Err(Error::rejected(
                    "app result requires its assigned native worker, not token possession",
                ));
            }
        }
        let mut agent = self.store.agent(&message.alias)?;
        if message.turn_id.as_deref() != Some(token.as_str()) {
            return Err(Error::rejected(
                "Token does not match the message's submission token",
            ));
        }
        // A recovered turn is neither stale nor valid until the actor
        // has proved (or refused) its pane. Park only the exact recovered
        // message/token, after all caller and identity checks, without
        // holding the store lock or mutating message state.
        if self.adoption_pending(&message.alias, &id, &token) {
            if !self.wait_for_adoption(
                &message.alias,
                &id,
                &token,
                Instant::now() + Duration::from_secs(20),
            ) {
                return Err(Error::rejected(
                    "Timed out waiting for running-turn re-adoption",
                ));
            }
            message = self
                .store
                .message(&id)?
                .ok_or_else(|| Error::rejected("Unknown message"))?;
            if message.turn_id.as_deref() != Some(token.as_str()) {
                return Err(Error::rejected(
                    "Token does not match the message's submission token",
                ));
            }
            agent = self.store.agent(&message.alias)?;
        }
        if !registry::turn_token_current(
            &agent.provider,
            &agent.endpoint_kind,
            agent.generation.as_deref(),
            &token,
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
        // CAD-1420: a bootstrap ACK validates and applies BEFORE any activity
        // bump, so a refused profile mutation changes neither activity nor the
        // ack. An ordinary ACK (no `bootstrap`) takes the unchanged path below.
        if kind == "ack" {
            if let Some(bootstrap) = params.get("bootstrap") {
                let AgentCaller::Agent(alias) = self.agent_caller(peer_pid, "bootstrap ack")?
                else {
                    return Err(Error::rejected(
                        "bootstrap ack refused: it needs the assistant's own connection",
                    ));
                };
                if alias != message.alias {
                    return Err(Error::rejected(
                        "bootstrap ack refused: the connection is not the message's assistant",
                    ));
                }
                self.bootstrap_ack(&message, &token, bootstrap)?;
            }
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
                    let completion = if message.source == "app_run_dispatch" {
                        let (run, _) = self
                            .store
                            .app_message_installation(&message.id)?
                            .ok_or_else(|| {
                                Error::rejected("app completion association is absent")
                            })?;
                        self.with_app_run_current(&run, |digest| {
                            self.store.app_message_admit(&message, digest)?;
                            self.store
                                .finish_running(&message.id, "completed", &stored, None)
                        })?
                    } else {
                        self.store
                            .finish_running(&message.id, "completed", &stored, None)?
                    };
                    match completion {
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
                )));
            }
        }
        self.wake();
        Ok(json!({"state": "reported", "kind": kind}))
    }

    /// CAD-1420: apply the closed bootstrap object on a stamped assistant ACK.
    /// Validation and the HTTP-capable reprove run BEFORE any SQLite lock. The
    /// store then re-checks message/turn/stamp/binding/revision in one fenced
    /// transaction and only then runs the local profile-file effect. Any
    /// refusal changes neither profile files nor lifecycle state.
    fn bootstrap_ack(
        self: &Arc<Self>,
        message: &store::Message,
        token: &str,
        bootstrap: &Value,
    ) -> Result<()> {
        let object = bootstrap
            .as_object()
            .ok_or_else(|| Error::rejected("bootstrap must be a JSON object"))?;
        const FIELDS: [&str; 3] = ["action", "expected_revision", "profile"];
        if let Some(field) = object.keys().find(|k| !FIELDS.contains(&k.as_str())) {
            return Err(Error::rejected(format!(
                "bootstrap refused: field '{field}' is not accepted"
            )));
        }
        let action = required_str(bootstrap, "action")?;
        if !matches!(action, "update" | "complete" | "skip" | "defer" | "resume") {
            return Err(Error::rejected("bootstrap action is not recognized"));
        }
        let expected_revision = bootstrap
            .get("expected_revision")
            .and_then(Value::as_i64)
            .filter(|r| *r >= 0)
            .ok_or_else(|| Error::rejected("bootstrap needs a nonnegative expected_revision"))?;
        let profile = match bootstrap.get("profile") {
            None => None,
            Some(value) => Some(bootstrap_files::Profile::parse(
                value
                    .as_object()
                    .ok_or_else(|| Error::rejected("bootstrap profile must be an object"))?,
            )?),
        };
        let stamp = self
            .store
            .message_member_authority(&message.id)?
            .ok_or_else(|| {
                Error::rejected("bootstrap refused: the message has no member authority")
            })?;
        if message.source == "assist_job_event" {
            return Err(Error::rejected(
                "bootstrap refused: a job notice cannot update the assistant profile",
            ));
        }
        // HTTP-capable revalidation of the ORIGINAL queued credential, outside
        // every SQLite lock.
        self.member_message_reprove(message)?;
        let expect = store::AssistDispatchExpect {
            org: stamp.org.clone(),
            sub: stamp.sub.clone(),
            assistant_id: stamp.assistant_id.clone(),
            alias: message.alias.clone(),
            credential_id: Some(stamp.credential_id.clone()),
            job: String::new(),
        };
        let root = self.assistant_principal_cwd(&stamp.org, &stamp.sub);
        self.store.assistant_bootstrap_apply(
            &expect,
            &message.id,
            token,
            action,
            expected_revision,
            || {
                if let Some(p) = profile.as_ref() {
                    bootstrap_files::write_profile(&root, p)?;
                }
                Ok(())
            },
        )?;
        // Completion is committed; cleanup is best-effort and never reopens setup.
        if action == "complete" {
            if let Err(error) = bootstrap_files::remove_bootstrap(&root) {
                eprintln!("bootstrap cleanup for {}: {error}", message.alias);
            }
        }
        Ok(())
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
        self.rpc_interrupt_bound(params, peer_pid, None)
    }

    /// CAD-1700: `rpc_interrupt`, optionally bound to one message. With
    /// `expected`, the turn must still be that exact running message when
    /// the interrupt is about to reach the adapter; otherwise the call is
    /// refused, so a stale click never stops a newer turn.
    pub(super) fn rpc_interrupt_bound(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
        expected: Option<&str>,
    ) -> Result<Value> {
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
        // — a refusal included.
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
        // CAD-1700: a bound stop re-checks the running turn after every point
        // it could wait (including the test pause above) and before the
        // adapter is asked, so a turn that changed since the caller named it
        // is refused and the newer turn is never interrupted.
        if let Some(expected) = expected {
            let still = self.store.running_message(&alias)?;
            if still.as_ref().map(|m| m.id.as_str()) != Some(expected) {
                let error = format!(
                    "message {expected} is no longer the master's running turn — nothing was stopped"
                );
                record("refused", Some(&error))?;
                return Err(Error::invalid("turn_not_running", error));
            }
        }
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

#[cfg(test)]
mod message_list_admit {
    use super::admit_message_list;
    use crate::daemon::caller_rule::Who;

    #[test]
    fn only_the_operator_or_the_recipient_may_list() {
        assert!(admit_message_list(&Who::Operator, "worker").is_ok());
        assert!(admit_message_list(&Who::Agent("worker".into()), "worker").is_ok());
        let other = admit_message_list(&Who::Agent("peer".into()), "worker").unwrap_err();
        assert!(other.to_string().contains("cannot list another"), "{other}");
        let unproven = admit_message_list(&Who::Unproven("no pane".into()), "worker").unwrap_err();
        assert!(unproven.to_string().contains("unproven"), "{unproven}");
    }
}
