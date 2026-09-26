//! The master agent's daemon side (CAD-339): starting it from its agent
//! files, the operator-only agent-file writer, the master policy every
//! RPC passes through, the master's own dispatch and escalation verbs,
//! the report router that brings workers' reports and unanswered
//! questions into the master's thread, and the "since you left" summary.
//!
//! The master is the agent whose alias is [`crate::master::ALIAS`]; the
//! daemon recognises it on a connection only through the one identity
//! verifier ([`Shared::caller_identity`], CAD-381) — never from request
//! fields or `CADENCE_ALIAS`. The policy is an **allowlist**: a master
//! connection reaches only [`MASTER_ALLOWED`]; every other method,
//! including any added later, is refused. Every check here is a process
//! guard, not a security boundary: a same-uid process that escapes the
//! master's process tree (setsid + double fork) is not recognised as the
//! master — CAD-276's residual, tracked for all agents in CAD-384.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    optional_str, optional_strs, reject_identity_fields, required_str, Caller, Shared, DAEMON_ALIAS,
};
use crate::client;
use crate::error::{Error, Result};
use crate::issue::{self, task_report};
use crate::master::{self, ALIAS};
use crate::peer::AgentCaller;
use crate::store;

/// The daemon methods a master connection may call — everything else is
/// refused (review round 1, C2). Reads, proposing a plan, registering a
/// project (`project_new`, CAD-358), its own
/// dispatch/escalate/summary verbs, a report on its own message,
/// `interrupt` — only of a turn it dispatched (CAD-323, checked in
/// `rpc_interrupt`) — and `answer_route` (CAD-447) — only of an answer
/// it filed itself, and only to that question's author.
pub const MASTER_ALLOWED: &[&str] = &[
    "health",
    "daemon_info",
    "agent_list",
    "agent_show",
    "agent_events",
    "thread_read",
    "job_list",
    "job_show",
    "job_events",
    "task_show",
    "model_defaults_get",
    // CAD-552: the reads behind the completed verb allowlist — `issue
    // ls --json`'s work block (`project_work_approvals`) and the
    // `overview`/`status` sections (`monitor_*`, `agent_probe`) — all
    // Rule::Read. Self-scoped reads like `agent_requests` stay refused:
    // empty would pass for "nothing pending".
    "project_work_approvals",
    "monitor_list",
    "monitor_alerts",
    "agent_probe",
    "plan_propose",
    // CAD-358: register a repo and seed PROJECT.md (tracker and state
    // dir refused; identity fields refused).
    "project_new",
    "master_dispatch",
    "question_escalate",
    "master_summary",
    "message_report",
    "interrupt",
    // CAD-447: route the master's own answer to the question's author
    // (only an answer it filed, only to that question's author).
    "answer_route",
    // CAD-431: read the review loop (never file a verdict or decide).
    "delivery_list",
    // CAD-615: file a permission request, peek a grant, and retry an
    // approved command. Deciding a request is not on this list.
    "master_ask_permission",
    "master_peek_grant",
    "master_permission_use",
];

/// Most reports one router pass queues to the master; the rest wait for
/// the next pass and are counted as the routing backlog.
const ROUTES_PER_PASS: usize = 5;
/// Default wait before an unanswered question reaches the master.
/// The checkup (CAD-477) gives a report the same grace before it
/// backstops an absent PM with an operator escalation.
pub(super) const QUESTION_GRACE_SECS: u64 = 900;
/// Body bytes a routed report carries; the file keeps the rest.
const ROUTED_BODY_MAX: usize = 6_000;
/// Briefing bytes the bootstrap message inlines; a larger briefing is
/// referenced by path.
const BOOTSTRAP_INLINE_MAX: usize = 40_000;

fn now_epoch() -> i64 {
    crate::issue::time::now_epoch()
}

/// `preferred` / `fallbacks` of AGENT.md's frontmatter: the provider,
/// model and effort the master launches with unless the operator names
/// them. Unreadable frontmatter just yields nothing.
fn preferred(agent_md: &str) -> Vec<(String, Option<String>, Option<String>)> {
    let Ok((yaml, _)) = crate::issue::parse::split_front(agent_md) else {
        return vec![];
    };
    let Ok(front) = serde_yaml::from_str::<Value>(yaml) else {
        return vec![];
    };
    let one = |v: &Value| {
        Some((
            v["provider"].as_str()?.to_string(),
            v["model"].as_str().map(str::to_string),
            v["effort"].as_str().map(str::to_string),
        ))
    };
    let mut out: Vec<_> = one(&front["preferred"]).into_iter().collect();
    if let Some(list) = front["fallbacks"].as_array() {
        out.extend(list.iter().filter_map(one));
    }
    out
}

/// Which provider a `master_start` launches: the explicit request wins;
/// a bare start resolves AGENT.md's `preferred.provider`, then each
/// `fallbacks` entry, taking the first the daemon accepts
/// ([`master::PROVIDERS`]); none usable → the first accepted provider
/// (CAD-322 round 2, I2).
fn resolve_provider(
    requested: Option<&str>,
    prefs: &[(String, Option<String>, Option<String>)],
) -> String {
    if let Some(p) = requested {
        return p.to_string();
    }
    prefs
        .iter()
        .map(|c| c.0.as_str())
        .find(|p| master::PROVIDERS.contains(p))
        .unwrap_or(master::PROVIDERS[0])
        .to_string()
}

fn short_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{} …[truncated — the report file has the rest]",
        &text[..end]
    )
}

/// Is `method` open to a master connection.
pub(crate) fn master_may_call(method: &str) -> bool {
    MASTER_ALLOWED.contains(&method)
}

impl Shared {
    /// Is a master registered at all — every master check is skipped,
    /// at no cost, on an install without one.
    pub(super) fn master_exists(&self) -> bool {
        self.store.agent_opt(ALIAS).ok().flatten().is_some()
    }

    /// Is this connection the master's (its process tree, per the one
    /// identity verifier). An underivable identity is not the master.
    pub(super) fn caller_is_master(&self, peer_pid: u32) -> bool {
        self.master_exists()
            && matches!(
                self.caller_identity(peer_pid),
                Ok(Caller::Agent(v)) if master::is_master(&v.agent.alias)
            )
    }

    /// The master's and the operator's verbs (escalate, posting the
    /// summary): the caller as #221's shared derivation
    /// ([`Shared::agent_caller`]) names it — the master's connection, or
    /// the proven operator; any other agent is refused. Identity-shaped
    /// request fields are refused, never read. Returns who acted.
    fn master_or_operator(
        &self,
        params: &Value,
        peer_pid: u32,
        verb: &str,
    ) -> Result<&'static str> {
        super::reject_identity_fields(params, verb)?;
        match self.agent_caller(peer_pid, verb)? {
            AgentCaller::Operator => Ok("operator"),
            AgentCaller::Agent(alias) if master::is_master(&alias) => Ok(ALIAS),
            AgentCaller::Agent(alias) => Err(Error::rejected(format!(
                "{verb} is an operator action (or the master's) — this connection is \
                 agent '{alias}'"
            ))),
        }
    }

    /// CAD-339: run before every RPC. Nobody but `master_start` registers
    /// the alias `master`; a master connection reaches only
    /// [`MASTER_ALLOWED`], and its `message_report` only for its own
    /// messages. A refusal happens before the method runs, so it leaves
    /// no write.
    pub(super) fn master_policy(&self, method: &str, params: &Value, peer_pid: u32) -> Result<()> {
        if method == "agent_register" && optional_str(params, "alias") == Some(ALIAS) {
            return Err(Error::invalid(
                "master_reserved",
                "the alias 'master' is reserved — only `cadence master start` registers it",
            ));
        }
        if !self.caller_is_master(peer_pid) {
            return Ok(());
        }
        if !master_may_call(method) {
            return Err(Error::invalid(
                "master_refused",
                format!(
                    "the master may not call {method} — it reads, proposes plans, dispatches \
                     approved tickets (`cadence master dispatch`), answers and escalates \
                     questions; everything else is the operator's"
                ),
            ));
        }
        if method == "message_report" {
            let id = required_str(params, "message")?;
            let own = self.store.message(id)?.is_some_and(|m| m.alias == ALIAS);
            if !own {
                return Err(Error::invalid(
                    "master_refused",
                    format!("the master reports only on its own messages, not {id}"),
                ));
            }
        }
        Ok(())
    }

    /// `master_dispatch` — the master's only way to hand work to an
    /// agent (review round 1, I1). The daemon, not the master, decides
    /// what is sent: the issue must be a ticket of an approved plan
    /// ([`crate::issue::plan::gate_master`]), `ready`, with every
    /// `blocked_by` done or dropped; it goes to the ticket's own agent
    /// (`owner`, set from the plan's `agent:`), or to `to` only when the
    /// ticket names none; and the kickoff is the standard one composed
    /// from the ticket (its `issue.md` is the note). A ticket dispatches
    /// once: dispatch moves it to `doing`, and every master dispatch runs
    /// under `dispatch_lock`, check included.
    pub(super) fn rpc_master_dispatch(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        super::reject_identity_fields(params, "master dispatch")?;
        if !self.caller_is_master(peer_pid) {
            return Err(Error::invalid(
                "master_only",
                "master_dispatch is the master's verb — others use `cadence dispatch`",
            ));
        }
        let id = required_str(params, "issue")?;
        // One master dispatch at a time (review round 2): the ticket is
        // read — `ready`, blockers done — under the same lock the
        // dispatch runs under, so a concurrent second call sees `doing`
        // and is refused having written nothing. The dispatch's own
        // daemon calls (agent_show, dispatch_send) never take this lock.
        let _serial = self.dispatch_lock.lock().unwrap_or_else(|e| e.into_inner());
        // CAD-431: a dispatch the review loop cannot record does not
        // happen — an unreadable delivery.json refuses before anything.
        crate::delivery::load(&self.state_dir).map_err(|e| {
            Error::invalid(
                "delivery_unreadable",
                format!("{e} — the operator repairs it before the master dispatches"),
            )
        })?;
        let pm = self.pm()?;
        let ticket = issue::board::find_issue(&pm.dir, id)?;
        let front = &ticket.front;
        crate::issue::plan::gate_master(&pm.dir, front, &ticket.body)?;
        if front.status != "ready" {
            return Err(Error::invalid(
                "master_dispatch_state",
                format!(
                    "{id} is {} — the master dispatches a ticket once, from ready",
                    front.status
                ),
            ));
        }
        for blocker in &front.blocked_by {
            let b = issue::board::find_issue(&pm.dir, blocker)?;
            if !matches!(b.front.status.as_str(), "done" | "dropped") {
                return Err(Error::invalid(
                    "master_dispatch_blocked",
                    format!(
                        "{id} depends on {blocker}, which is {} — dispatch it once that is done",
                        b.front.status
                    ),
                ));
            }
        }
        let to = match (front.owner.as_deref(), optional_str(params, "to")) {
            (Some(owner), None) => owner.to_string(),
            (Some(owner), Some(to)) if owner == to => owner.to_string(),
            (Some(owner), Some(to)) => {
                return Err(Error::invalid(
                    "master_dispatch_target",
                    format!(
                        "{id} is assigned to {owner} — the master cannot send it to {to}; \
                         ask the operator to reassign it"
                    ),
                ))
            }
            (None, Some(to)) => to.to_string(),
            (None, None) => {
                return Err(Error::invalid(
                    "master_dispatch_target",
                    format!("{id} names no agent — pass --to <alias>"),
                ))
            }
        };
        if master::is_master(&to) {
            return Err(Error::invalid(
                "master_dispatch_target",
                "the master never dispatches to itself — it does not implement",
            ));
        }
        let agent = self.store.agent(&to)?;
        if agent.state == "attention" {
            return Err(Error::rejected(format!(
                "{to} is fenced — the operator reconciles it before work goes there"
            )));
        }
        let args = issue::dispatch::DispatchArgs {
            to: to.clone(),
            note: None,
            name: None,
            base: None,
            repo: None,
            reply_to: Some(ALIAS.to_string()),
            summary: None,
            job_spec: None,
            no_lessons: false,
            force: false,
            take_over: None,
        };
        // The daemon runs the ordinary dispatch on the master's behalf:
        // worktree, tracker refs and comment, one kickoff — attributed
        // to the master.
        // CAD-482: its internal `dispatch_send` call goes back over the
        // socket with the daemon itself as peer — under the test seam it
        // asserts the operator identity an operator-launched daemon
        // proves in production, rather than leaning on the daemon's own
        // ambient ancestry (which a test pane marks as an agent's).
        let out = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            issue::dispatch::run(&pm, id, &args, ALIAS, &self.state_dir, Some(ALIAS))
        })?;
        // Only a real send is the master's dispatch: a duplicate answers
        // with the live kickoff someone else sent (`dispatched: false`),
        // and recording that would hand the master interrupt rights over
        // it (CAD-323).
        if out["dispatched"] != json!(false) {
            // CAD-431: the ticket enters the review loop; its worker's
            // done report is what moves it on.
            if let Err(e) = self.delivery_start(id, &ticket.project, &to) {
                tracing::warn!("delivery record for {id}: {e}");
            }
            let _ = self.store.event_public(
                DAEMON_ALIAS,
                "master_dispatched",
                json!({"issue": id, "to": to, "message": out["message"]}),
            );
        }
        Ok(out)
    }

    /// `question_escalate` — hand an open question the master cannot
    /// answer to the operator (review round 1, I3). Only the master's
    /// connection (or the proven operator) may; the record lives in the
    /// daemon's state dir, which is the only source the operator's
    /// Needs-you reads — a report file can never put a question there.
    /// A tracker comment keeps the human record on the ticket.
    pub(super) fn rpc_question_escalate(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let by = self.master_or_operator(params, peer_pid, "question escalate")?;
        let id = required_str(params, "issue")?;
        let question = required_str(params, "question")?;
        let summary = required_str(params, "summary")?.trim();
        if summary.is_empty() || summary.len() > master::ESCALATION_SUMMARY_MAX {
            return Err(Error::rejected(format!(
                "the summary is 1-{} bytes",
                master::ESCALATION_SUMMARY_MAX
            )));
        }
        crate::secret::guard(&format!("{id}: escalation"), summary)?;
        let pm = self.pm()?;
        let ticket = issue::board::find_issue(&pm.dir, id)?;
        let open = task_report::open_questions(&ticket.dir, id)
            .into_iter()
            .any(|q| q["name"] == question);
        if !open {
            return Err(Error::rejected(format!(
                "{id} has no open question '{question}' — `cadence issue show {id}` lists its reports"
            )));
        }
        let key = format!("{id}/{question}");
        let record = json!({
            "issue": id, "question": question, "summary": summary,
            "by": by, "at": crate::issue::time::iso(now_epoch()),
        });
        {
            let _guard = self
                .escalation_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            master::record_escalation(&self.state_dir, &key, record.clone())?;
        }
        let _ = issue::write::add_comment(
            &pm,
            id,
            &format!("Question {question} escalated to the operator by {by}:\n\n{summary}"),
            Some(by),
            None,
            None,
            by,
        );
        let _ = self
            .store
            .event_public(DAEMON_ALIAS, "question_escalated", record.clone());
        self.wake();
        Ok(record)
    }

    /// `agent_file_write` — the one writer of an agent's SOUL.md and
    /// AGENT.md, for the proven operator only. The file's digest is
    /// recorded so a later edit around this writer is caught at
    /// `master start`.
    pub(super) fn rpc_agent_file_write(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("agent file write", params, peer_pid)?;
        let slug = required_str(params, "agent")?;
        let name = required_str(params, "file")?;
        let text = required_str(params, "text")?;
        let pm = self.pm()?;
        let out = master::write_file(&pm, slug, name, text, "operator")?;
        master::record(&self.state_dir, slug, name, &master::digest(text))?;
        Ok(out)
    }

    /// `master_start` — operator only. Installs any missing default agent
    /// file, refuses files changed around the writer, then registers the
    /// one `master` agent — managed Claude, in an empty working dir under
    /// the state dir — and queues its bootstrap: the briefing built from
    /// SOUL.md + AGENT.md. Its thread starts here, so reports routed to
    /// it land in the chat.
    pub(super) fn rpc_master_start(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("master start", params, peer_pid)?;
        if let Some(agent) = self.store.agent_opt(ALIAS)? {
            return Err(Error::invalid(
                "master_exists",
                format!(
                    "the master is already registered ({} {}, {}) — one per install; \
                     `cadence agent resume master`, or stop and remove it first",
                    agent.provider, agent.endpoint_kind, agent.state
                ),
            ));
        }
        let pm = self.pm()?;
        // Files edited around the writer refuse BEFORE anything is
        // installed or registered — a refusal writes nothing.
        master::verify(
            &self.state_dir,
            ALIAS,
            &master::read_files(&pm.dir, ALIAS, false)?,
        )?;
        // `claude` and `pi` (CAD-322) — a Codex master still waits for a
        // read-only sandbox with its writes through daemon verbs.
        let requested = optional_str(params, "provider");
        if let Some(p) = requested.filter(|p| !master::PROVIDERS.contains(p)) {
            return Err(Error::invalid(
                "master_provider",
                format!(
                    "the master runs on {} only for now, not '{p}': a {p} master needs a \
                     read-only sandbox with its writes going through daemon verbs (follow-up)",
                    master::PROVIDERS.join(" or ")
                ),
            ));
        }
        // CAD-439: the master runs confined. On a host that cannot, it
        // starts only with the operator's explicit `--unconfined`; that
        // flag is refused where confinement works. Both refuse before
        // anything is installed or registered.
        let unconfined = params
            .get("unconfined")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let copy = params
            .get("copy_login")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if copy && unconfined {
            return Err(Error::invalid(
                "master_copy_login",
                "--copy-login is for a confined master's own config dir; an unconfined \
                 master uses the operator's Claude config as it is",
            ));
        }
        match (
            master::confinement_available(&self.provider_env),
            unconfined,
        ) {
            (Ok(()), false) | (Err(_), true) => {}
            (Ok(()), true) => {
                return Err(Error::invalid(
                    "master_unconfined",
                    "this host can confine the master (Landlock); --unconfined is only for \
                     hosts without it",
                ))
            }
            (Err(e), false) => {
                return Err(Error::invalid(
                    "master_unconfined",
                    format!(
                        "{e}. The master cannot be confined on this host, so it is not \
                         started. `cadence master start --unconfined` starts it without the \
                         sandbox — it can then read and write your files (ssh keys, forge \
                         logins, every repo)"
                    ),
                ))
            }
        }
        let installed = master::install_defaults(&pm, "operator")?;
        let files = master::read_files(&pm.dir, ALIAS, true)?;
        master::verify(&self.state_dir, ALIAS, &files)?;
        let agent_md = files
            .iter()
            .find(|(n, _)| n == "AGENT.md")
            .map(|(_, t)| t.as_str())
            .unwrap_or_default();
        // The provider launched: an explicit `--provider` wins; a bare
        // `master start` resolves AGENT.md's `preferred.provider` (then
        // its `fallbacks`), falling back to the first provider the
        // daemon accepts when none of the configured ones is usable
        // (CAD-322 round 2, I2).
        let prefs = preferred(agent_md);
        let provider = resolve_provider(requested, &prefs);
        let choice = prefs.iter().find(|c| c.0 == provider);
        let model = optional_str(params, "model")
            .map(str::to_string)
            .or_else(|| choice.as_ref().and_then(|c| c.1.clone()));
        // CAD-559: a pi master always launches on an explicit allowlisted
        // model — `--model`, then AGENT.md's `preferred`, then
        // `[pi].models.default.master`, else refuse; Pi's own fallback
        // is never used. Whatever wins must be on `[pi].models.allow`.
        let mut pi_selection = None;
        let model = if provider == "pi" {
            let policy = crate::pi_policy::read(&pm.dir)?;
            let chosen = model.as_deref();
            let resolved = crate::pi_policy::resolve_model(policy.as_ref(), "master", chosen)?;
            if chosen.is_none() {
                // pm.yaml's role default filled the slot — `register_agent`
                // will label it `explicit`, so restamp the real
                // provenance after the row lands.
                pi_selection = Some(crate::model_defaults::pi_policy_default_selection(
                    "master", &resolved,
                ));
            }
            Some(resolved)
        } else {
            model
        };
        let effort = optional_str(params, "effort")
            .map(str::to_string)
            .or_else(|| choice.as_ref().and_then(|c| c.2.clone()));
        let mut launch = serde_json::Map::new();
        if let Some(m) = &model {
            launch.insert("model".into(), json!(m));
        }
        if let Some(e) = &effort {
            launch.insert("effort".into(), json!(e));
        }
        if unconfined {
            launch.insert("unconfined".into(), json!(true));
        }
        let launch = Value::Object(launch);
        crate::adapter::registry::validate_launch_params(&provider, "managed", &launch)?;
        let briefing = master::compose(&files);
        let file = client::briefing_path(&self.state_dir, &Value::Null, ALIAS);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&file, &briefing)?;
        // An empty working dir the daemon owns — never the tracker or a
        // repo, where any agent can plant CLAUDE.md, hooks or settings.
        let cwd = master::workdir(&self.state_dir);
        std::fs::create_dir_all(&cwd)?;
        // Confined, the master's CLI has its own config dir (CAD-439
        // review, I1) and, by default, its own separate login: nothing is
        // copied unless the operator asks (`--copy-login`). Unconfined,
        // it keeps the operator's config.
        let login = if unconfined {
            None
        } else if copy {
            let operator = master::operator_provider_config(
                &provider,
                self.provider_env
                    .var(crate::master::provider_config_env(&provider)),
                self.provider_env.var("HOME"),
            );
            Some(match operator {
                Some(dir) => master::copy_login_for(&provider, &self.state_dir, &dir)?,
                None => master::ensure_config_dir_for(&provider, &self.state_dir)?,
            })
        } else {
            Some(master::ensure_config_dir_for(&provider, &self.state_dir)?)
        };
        let login_command = (login == Some(master::Login::None))
            .then(|| master::login_command_for(&provider, &self.state_dir));
        self.store.register_agent(&store::NewAgent {
            alias: ALIAS,
            provider: &provider,
            endpoint_kind: "managed",
            role: "worker",
            cwd: &cwd.canonicalize()?.to_string_lossy(),
            sandbox: "read-only",
            instructions: Some(&briefing),
            params: Some(&launch.to_string()),
            team_role: None,
            model_policy: None,
        })?;
        // CAD-559: pm.yaml's `[pi].models.default.master` filled the
        // model — restamp the honest provenance (`register_agent`
        // derived `explicit` from the merged params).
        if let Some(selection) = &pi_selection {
            self.store.set_model_selection(ALIAS, selection)?;
        }
        let thread = self.store.ensure_thread(ALIAS)?;
        if let Err(e) = self.launch_actor(ALIAS) {
            // No half-started master: the row goes with its launch.
            let _ =
                self.store
                    .remove_agent(ALIAS, true, &super::caller_audit(&AgentCaller::Operator));
            return Err(e);
        }
        let body = if briefing.len() <= BOOTSTRAP_INLINE_MAX {
            format!(
                "Cadence bootstrap: you are 'master', the operator's company assistant. \
                 Your briefing follows (also on disk at {}). Reply to the operator in this \
                 thread; your reply text is what they read.\n\n{briefing}",
                file.display()
            )
        } else {
            format!(
                "Cadence bootstrap: you are 'master', the operator's company assistant. \
                 Your briefing is on disk at {} and too long to inline; ask the operator \
                 to shorten SOUL.md/AGENT.md. Reply to the operator in this thread.",
                file.display()
            )
        };
        self.send_as(
            &json!({"alias": ALIAS, "text": body, "message": "bootstrap-master",
                    "source": "bootstrap"}),
            &|_| Ok(store::Sender::Unattributed),
        )?;
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "master_started",
            json!({"provider": &provider, "model": model, "installed": installed,
                   "confined": !unconfined,
                   "login": login.map(master::Login::as_str)}),
        );
        if login == Some(master::Login::Copied) {
            let _ = self.store.event_public(
                ALIAS,
                "master_login_copied",
                json!({"by": "operator", "provider": &provider,
                       "to": master::provider_config_dir(&provider, &self.state_dir)}),
            );
        }
        if unconfined {
            let _ = self.store.event_public(
                ALIAS,
                "master_started_unconfined",
                json!({"by": "operator", "reason": "no filesystem sandbox on this host"}),
            );
        }
        self.wake();
        Ok(json!({
            "confined": !unconfined,
            "login": login.map(master::Login::as_str),
            "login_command": login_command,
            "warning": unconfined.then_some(master::UNCONFINED_WARNING),
            "alias": ALIAS,
            "provider": provider,
            "endpoint_kind": "managed",
            "model": model,
            "effort": effort,
            "state": "starting",
            "installed": installed,
            "agent_dir": master::agent_dir(&pm.dir, ALIAS),
            "cwd": cwd,
            "briefing": file,
            "thread": thread.to_json(),
        }))
    }

    /// `master_summary` — the "since you left" summary (`since`: epoch,
    /// ISO or a look-back like `24h`). `post` appends it to the master's
    /// thread as a system entry: only the master or the proven operator
    /// posts.
    pub(super) fn rpc_master_summary(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let since = match params.get("since") {
            Some(Value::Number(n)) => n
                .as_i64()
                .ok_or_else(|| Error::rejected("'since' must be whole seconds"))?,
            Some(Value::String(s)) => issue::summary::parse_since(s, now_epoch())?,
            _ => return Err(Error::rejected("Missing 'since'")),
        };
        let post = params.get("post").and_then(Value::as_bool).unwrap_or(false);
        if post {
            if !self.master_exists() {
                return Err(Error::rejected(
                    "no master to post to — `cadence master start` first",
                ));
            }
            self.master_or_operator(params, peer_pid, "posting into the master's thread")?;
        }
        let pm = self.pm()?;
        let escalated = master::escalations(&self.state_dir);
        let mut out = issue::summary::since(&pm, since, &escalated)?;
        out["routing_backlog"] = json!(self.router_backlog.load(Ordering::SeqCst));
        if post {
            self.store.ensure_thread(ALIAS)?;
            let seq = self.store.thread_append(
                ALIAS,
                store::NewEntry {
                    role: store::ROLE_SYSTEM,
                    kind: store::KIND_MESSAGE,
                    text: out["text"].as_str().unwrap_or_default(),
                    payload: Some(json!({"event": "since_summary", "since": out["since"]})),
                    message_id: None,
                },
            )?;
            out["posted"] = json!(seq);
            self.wake();
        }
        Ok(out)
    }

    /// `reports_changed` — the operator's hint that a report was filed;
    /// the router scans at once instead of at its next period. Anyone
    /// else waits for the period.
    pub(super) fn rpc_reports_changed(&self, peer_pid: u32) -> Result<Value> {
        self.proven_operator("reports changed", peer_pid)?;
        self.reports_dirty.store(true, Ordering::SeqCst);
        Ok(json!({"ok": true}))
    }

    /// The report router's thread: a scan every period, or at once after
    /// an operator's `reports_changed`.
    pub(super) fn run_report_router(self: &Arc<Self>) {
        let Some(every) = self.router_every else {
            return;
        };
        let mut next = Instant::now();
        while !self.closing.load(Ordering::SeqCst) {
            if self.reports_dirty.swap(false, Ordering::SeqCst) || Instant::now() >= next {
                if let Err(e) = self.route_reports() {
                    tracing::debug!("report router: {e}");
                }
                if let Err(e) = self.route_delivery() {
                    tracing::debug!("delivery router: {e}");
                }
                if let Err(e) = self.route_wakes() {
                    tracing::debug!("master wakes: {e}");
                }
                next = Instant::now() + every;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }

    /// One router pass (CAD-339). With a master registered:
    ///
    /// - every `done` / `blocked` report filed after the master started
    ///   (and not by it) → one message to the master, so it lands in the
    ///   master's thread and the master can dispatch what is next;
    /// - every open, unescalated question older than `[host]
    ///   question_escalate_after_secs` (default 900 — the window a PM has
    ///   to answer), including questions open before the master started
    ///   → one message with the question and how to answer or escalate.
    ///
    /// Each routes once (the message id is derived from the report path).
    /// At most [`ROUTES_PER_PASS`] are queued per pass — one master turn
    /// each; the rest wait and are counted in `router_backlog`.
    pub(super) fn route_reports(self: &Arc<Self>) -> Result<usize> {
        let Some(master_row) = self.store.agent_opt(ALIAS)? else {
            return Ok(0);
        };
        let pm_dir = self.pm_dir()?;
        if !pm_dir.join("pm.yaml").is_file() {
            return Ok(0);
        }
        let grace = crate::doctor::host::read_host_overrides(&pm_dir)
            .ok()
            .flatten()
            .and_then(|o| o.question_escalate_after_secs)
            .unwrap_or(QUESTION_GRACE_SECS) as i64;
        let baseline = master_row.created.floor() as i64;
        let escalated = master::escalations(&self.state_dir);
        let now = now_epoch();
        let mut due: Vec<(String, String, Value)> = Vec::new();
        for project in issue::project::list(&pm_dir)? {
            let Ok(entries) = std::fs::read_dir(pm_dir.join(&project.key)) else {
                continue;
            };
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().to_string();
                let dir = entry.path();
                if !issue::model::valid_id(&id)
                    || !issue::board::is_real_dir(&dir)
                    || task_report::names(&dir).is_empty()
                {
                    continue;
                }
                for row in task_report::list(&dir, &id) {
                    if !row["error"].is_null() || row["agent"].as_str() == Some(ALIAS) {
                        continue;
                    }
                    let Some(at) = row["at"].as_str().and_then(issue::time::parse_iso) else {
                        continue;
                    };
                    let name = row["name"].as_str().unwrap_or_default();
                    let route = match row["kind"].as_str() {
                        // A verdict reaches the master only from
                        // `report_verdict` (CAD-431) — a planted file
                        // under reports/ routes nowhere.
                        Some("done" | "blocked") => at >= baseline,
                        Some("question") => {
                            row["open"] == true
                                && !escalated.contains_key(&format!("{id}/{name}"))
                                && now - at >= grace
                        }
                        _ => false,
                    };
                    if route
                        && self
                            .store
                            .message(&route_id(&project.key, &id, &row))?
                            .is_none()
                    {
                        due.push((project.key.clone(), id.clone(), row));
                    }
                }
            }
        }
        // Oldest first, then a bounded batch.
        due.sort_by(|a, b| a.2["at"].as_str().cmp(&b.2["at"].as_str()));
        let mut routed = 0;
        for (project, id, row) in due.iter().take(ROUTES_PER_PASS) {
            if self.route_one(project, id, row)? {
                routed += 1;
            }
        }
        self.router_backlog
            .store(due.len().saturating_sub(routed), Ordering::SeqCst);
        Ok(routed)
    }

    /// Queue one routed report to the master; `false` when it was
    /// already routed.
    fn route_one(self: &Arc<Self>, project: &str, id: &str, row: &Value) -> Result<bool> {
        let name = row["name"].as_str().unwrap_or_default();
        let kind = row["kind"].as_str().unwrap_or_default();
        let agent = row["agent"].as_str().unwrap_or_default();
        let at = row["at"].as_str().unwrap_or_default();
        let path = format!("{project}/{id}/{}/{name}", task_report::DIR);
        let mid = route_id(project, id, row);
        if self.store.message(&mid)?.is_some() {
            return Ok(false);
        }
        let body = clip(
            row["body"].as_str().unwrap_or_default().trim(),
            ROUTED_BODY_MAX,
        );
        let text = if kind == "question" {
            let options = row["options"]
                .as_array()
                .map(|o| {
                    o.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .unwrap_or_default();
            format!(
                "[question] {id} from {agent}, open since {at} — no PM answered it.\n\
                 Report: {path}\nImpact: {}\nOptions: {options}\n\n{body}\n\n\
                 Answer it: `cadence report file --task {id} --kind answer` with frontmatter \
                 `answers: {name}`. If the ticket, the plan and the operator's words do not \
                 settle it: `cadence master escalate {id} {name} --file -` with a \
                 one-paragraph summary — the operator then sees it in Needs-you.",
                row["impact"].as_str().unwrap_or_default()
            )
        } else {
            format!("[report] {id} {kind} by {agent} at {at}\nReport: {path}\n\n{body}")
        };
        self.send_as(
            &json!({"alias": ALIAS, "text": text, "message": mid, "source": "report"}),
            &|_| Ok(store::Sender::Unattributed),
        )?;
        let _ = self.store.event_public(
            ALIAS,
            "report_routed",
            json!({"issue": id, "report": name, "kind": kind, "message": mid}),
        );
        Ok(true)
    }

    /// The master, by its connection. The operator files nothing here.
    fn require_master_caller(&self, peer_pid: u32, verb: &str) -> Result<()> {
        match self.agent_caller(peer_pid, verb)? {
            AgentCaller::Agent(alias) if master::is_master(&alias) && self.master_exists() => {
                Ok(())
            }
            AgentCaller::Agent(alias) => Err(Error::rejected(format!(
                "{verb} is the master's — this connection is agent '{alias}'"
            ))),
            AgentCaller::Operator => Err(Error::rejected(format!(
                "{verb} is the master's — the operator decides requests, and does not file them"
            ))),
        }
    }

    fn perm_roots(&self) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
        let mut states = vec![self.state_dir.clone()];
        if let Ok(prod) = crate::home::state_default() {
            if prod != self.state_dir {
                states.push(prod);
            }
        }
        let checkouts = self
            .pm()
            .ok()
            .and_then(|pm| crate::issue::project::list(&pm.dir).ok())
            .map(|projects| {
                projects
                    .into_iter()
                    .flat_map(|p| p.repos)
                    .filter_map(|r| r.path)
                    .filter_map(|p| std::fs::canonicalize(p).ok())
                    .collect()
            })
            .unwrap_or_default();
        (checkouts, states)
    }

    fn perm_states(states: &[std::path::PathBuf]) -> Vec<&std::path::Path> {
        states.iter().map(std::path::PathBuf::as_path).collect()
    }

    fn audit_perm(&self, kind: &str, payload: Value) {
        let _ = self.store.event_public(PERM_AUDIT_STREAM, kind, payload);
    }

    fn tell_master(&self, key: &str, text: &str) {
        let id = crate::proto::daemon_message_id("permission", key);
        let _ = self.store.enqueue_daemon(ALIAS, text, &id, "permission");
    }

    /// `master_ask_permission` — the master files one request for an
    /// exact argv. A duplicate pending request is returned as-is.
    pub(super) fn rpc_master_ask_permission(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.require_master_caller(peer_pid, "master ask-permission")?;
        reject_identity_fields(params, "master ask-permission")?;
        let argv = optional_strs(params, "argv")?;
        let cwd = std::path::PathBuf::from(required_str(params, "cwd")?);
        let reason = required_str(params, "reason")?;
        let (checkouts, states) = self.perm_roots();
        let state_refs = Self::perm_states(&states);
        let req = crate::master_perm::ask(
            &self.state_dir,
            &argv,
            &cwd,
            reason,
            ALIAS,
            &checkouts,
            &state_refs,
            crate::master_perm::clock(),
        )?;
        let body = crate::master_perm::request_json(&req);
        self.audit_perm("permission_requested", body.clone());
        self.tell_master(
            &format!("request/{}", req.id),
            &format!(
                "Permission requested ({}, risk {}): `{}`\nReason: {}\nThe operator can allow it once, always, or reject it.",
                req.id,
                body["risk"].as_str().unwrap_or("medium"),
                req.argv.join(" "),
                req.reason
            ),
        );
        self.wake();
        Ok(body)
    }

    /// `master_peek_grant` — does a live grant or allow rule cover this
    /// exact argv? Does not consume. The guard calls it.
    pub(super) fn rpc_master_peek_grant(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.require_master_caller(peer_pid, "master peek-grant")?;
        reject_identity_fields(params, "master peek-grant")?;
        let argv = optional_strs(params, "argv")?;
        let cwd = std::path::PathBuf::from(required_str(params, "cwd")?);
        let (checkouts, states) = self.perm_roots();
        let state_refs = Self::perm_states(&states);
        let pm = self.pm().ok();
        let allowed = crate::master_perm::peek(
            &self.state_dir,
            pm.as_ref().map(|p| p.dir.as_path()),
            &argv,
            &cwd,
            &checkouts,
            &state_refs,
            crate::master_perm::clock(),
        )?;
        if !allowed {
            return Err(Error::rejected("no live permission for that command"));
        }
        Ok(json!({"allowed": true}))
    }

    /// `master_permission_use` — the master retries the exact argv. A
    /// live grant is consumed (or an allow rule matches) and the
    /// daemon runs the command, returning its output. No grant is
    /// `applied: false`, so an allowlisted command continues normally.
    pub(super) fn rpc_master_permission_use(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.require_master_caller(peer_pid, "master permission use")?;
        reject_identity_fields(params, "master permission use")?;
        let argv = optional_strs(params, "argv")?;
        let cwd = std::path::PathBuf::from(required_str(params, "cwd")?);
        let (checkouts, states) = self.perm_roots();
        let state_refs = Self::perm_states(&states);
        let pm = self.pm().ok();
        let now = crate::master_perm::clock();
        let pm_dir = pm.as_ref().map(|p| p.dir.as_path());
        if !crate::master_perm::peek(
            &self.state_dir,
            pm_dir,
            &argv,
            &cwd,
            &checkouts,
            &state_refs,
            now,
        )? {
            return Ok(json!({"applied": false}));
        }
        let used = crate::master_perm::take(
            &self.state_dir,
            pm_dir,
            &argv,
            &cwd,
            &checkouts,
            &state_refs,
            now,
        )?;
        let mut out = self.run_approved(&argv, &cwd)?;
        out["applied"] = json!(true);
        out["use"] = match &used {
            crate::master_perm::Use::Grant { request_id } => json!({"grant": request_id}),
            crate::master_perm::Use::Rule { id } => json!({"rule": id}),
        };
        self.audit_perm(
            "permission_used",
            json!({"argv": argv, "cwd": cwd, "use": out["use"].clone(), "code": out["code"].clone()}),
        );
        self.tell_master(
            &format!("use/{}-{}", argv.join(" "), now),
            &format!(
                "Ran approved command (exit {}): `{}`",
                out["code"].as_i64().unwrap_or(1),
                argv.join(" ")
            ),
        );
        Ok(out)
    }

    /// `master_permission_allow_once` — operator only. One use of the
    /// exact argv and cwd.
    pub(super) fn rpc_master_permission_allow_once(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("master allow-once", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let (checkouts, states) = self.perm_roots();
        let state_refs = Self::perm_states(&states);
        let req = crate::master_perm::allow_once(
            &self.state_dir,
            id,
            &checkouts,
            &state_refs,
            crate::master_perm::clock(),
        )?;
        let body = crate::master_perm::request_json(&req);
        self.audit_perm(
            "permission_decided",
            json!({"id": id, "decision": "allow_once", "argv": req.argv}),
        );
        self.tell_master(
            &format!("decision/{id}"),
            &format!(
                "Operator allowed once: `{}` ({id}). Retry that exact command.",
                req.argv.join(" ")
            ),
        );
        self.wake();
        Ok(body)
    }

    /// `master_permission_always` — operator only. Saves a rule to
    /// `agents/master/permissions.yaml` and commits it as the operator.
    pub(super) fn rpc_master_permission_always(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("master always-allow", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let scope = match optional_str(params, "scope").unwrap_or("exact") {
            "exact" => crate::master_perm::Scope::Exact,
            "prefix" => crate::master_perm::Scope::Prefix,
            other => {
                return Err(Error::rejected(format!(
                    "scope is 'exact' or 'prefix', not '{other}'"
                )))
            }
        };
        let tail = optional_strs(params, "tail")?;
        let (checkouts, states) = self.perm_roots();
        let state_refs = Self::perm_states(&states);
        let (req, rule) = crate::master_perm::always_rule(
            &self.state_dir,
            id,
            scope,
            &tail,
            &checkouts,
            &state_refs,
            crate::master_perm::clock(),
        )?;
        let pm = self.pm()?;
        if let Err(e) = self.persist_rule(&pm, rule.clone()) {
            let _ = crate::master_perm::reopen(&self.state_dir, id, crate::master_perm::clock());
            return Err(e);
        }
        let body = json!({
            "request": crate::master_perm::request_json(&req),
            "rule": crate::master_perm::rule_json(&rule),
        });
        self.audit_perm(
            "permission_decided",
            json!({"id": id, "decision": "always", "rule": body["rule"].clone()}),
        );
        self.tell_master(
            &format!("decision/{id}"),
            &format!(
                "Operator always-allowed `{}` as rule {}.",
                req.argv.join(" "),
                rule.id
            ),
        );
        self.wake();
        Ok(body)
    }

    /// `master_permission_reject` — operator only. An optional deny
    /// rule (`dont_ask_again`) wins over later allow rules.
    pub(super) fn rpc_master_permission_reject(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("master reject", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let dont = params["dont_ask_again"].as_bool().unwrap_or(false);
        let (req, rule) =
            crate::master_perm::reject(&self.state_dir, id, dont, crate::master_perm::clock())?;
        if let Some(rule) = &rule {
            let pm = self.pm()?;
            if let Err(e) = self.persist_rule(&pm, rule.clone()) {
                let _ =
                    crate::master_perm::reopen(&self.state_dir, id, crate::master_perm::clock());
                return Err(e);
            }
        }
        self.audit_perm(
            "permission_decided",
            json!({"id": id, "decision": "reject", "dont_ask_again": dont, "argv": req.argv}),
        );
        self.tell_master(
            &format!("decision/{id}"),
            &format!(
                "Operator rejected `{}` ({id}){}.",
                req.argv.join(" "),
                if dont {
                    " and will not be asked again"
                } else {
                    ""
                }
            ),
        );
        self.wake();
        Ok(crate::master_perm::request_json(&req))
    }

    /// `master_permission_revoke` — operator only. The rule stops
    /// matching on the next check.
    pub(super) fn rpc_master_permission_revoke(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("master revoke-permission", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let pm = self.pm()?;
        let _lock = pm.lock()?;
        let rule = crate::master_perm::revoke_rule(&pm.dir, ALIAS, id)?;
        let path = crate::master_perm::rules_file(&pm.dir, ALIAS)?;
        crate::issue::write::commit(
            &pm,
            std::slice::from_ref(&path),
            "agents/master: revoke a permission rule",
            &[],
            "operator",
        )?;
        self.audit_perm("permission_revoked", crate::master_perm::rule_json(&rule));
        self.wake();
        Ok(crate::master_perm::rule_json(&rule))
    }

    /// `master_permission_list` — operator only. Pending requests and
    /// the rules file.
    pub(super) fn rpc_master_permission_list(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("master permissions", params, peer_pid)?;
        let pending =
            crate::master_perm::board_requests(&self.state_dir, crate::master_perm::clock())?;
        let rules = self
            .pm()
            .ok()
            .and_then(|pm| crate::master_perm::read_rules(&pm.dir, ALIAS).ok())
            .unwrap_or_default();
        Ok(json!({
            "requests": pending.iter().map(crate::master_perm::request_json).collect::<Vec<_>>(),
            "rules": rules.iter().map(crate::master_perm::rule_json).collect::<Vec<_>>(),
        }))
    }

    fn persist_rule(&self, pm: &crate::issue::Pm, rule: crate::master_perm::Rule) -> Result<()> {
        let _lock = pm.lock()?;
        let mut rules = crate::master_perm::read_rules(&pm.dir, ALIAS)?;
        rules.push(rule);
        let path = crate::master_perm::write_rules(&pm.dir, ALIAS, &rules)?;
        if let Err(e) = crate::issue::write::commit(
            pm,
            std::slice::from_ref(&path),
            "agents/master: update permissions.yaml",
            &[],
            "operator",
        ) {
            rules.pop();
            let _ = crate::master_perm::write_rules(&pm.dir, ALIAS, &rules);
            return Err(e);
        }
        Ok(())
    }

    /// Run an approved command. A read-only tool runs directly. A
    /// `cadence` verb is re-exec'd as a child of this daemon carrying
    /// a one-shot token; that child's connection is the operator for
    /// the life of the process, then the token is dropped.
    fn run_approved(&self, argv: &[String], cwd: &std::path::Path) -> Result<Value> {
        const CAP: usize = 32_000;
        let clip = |bytes: &[u8]| {
            let text = String::from_utf8_lossy(bytes);
            if text.len() <= CAP {
                return text.into_owned();
            }
            let mut end = CAP;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…[truncated]", &text[..end])
        };
        if crate::master_perm::readonly_tool(argv) {
            let bin = ["/bin", "/usr/bin"]
                .into_iter()
                .map(|d| std::path::PathBuf::from(d).join(&argv[0]))
                .find(|p| p.is_file())
                .ok_or_else(|| Error::rejected(format!("no {} under /bin or /usr/bin", argv[0])))?;
            let mut cmd = std::process::Command::new(bin);
            cmd.args(&argv[1..]).current_dir(cwd);
            scrub_grant_env(&mut cmd);
            let out = crate::reaper::output(&mut cmd).map_err(|e| {
                Error::rejected(format!("running the approved command failed: {e}"))
            })?;
            return Ok(json!({
                "code": out.status.code().unwrap_or(1),
                "stdout": clip(&out.stdout),
                "stderr": clip(&out.stderr),
            }));
        }
        if !argv.first().is_some_and(|a| {
            std::path::Path::new(a).file_name().and_then(|s| s.to_str()) == Some("cadence")
        }) {
            return Err(Error::rejected(
                "an approved command is a cadence verb or ls/cat/grep/find",
            ));
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        let exe = std::env::current_exe().map_err(|e| Error::internal(e.to_string()))?;
        let mut cmd = std::process::Command::new(exe);
        cmd.args(&argv[1..])
            .current_dir(cwd)
            .env("CADENCE_STATE_DIR", &self.state_dir);
        scrub_grant_env(&mut cmd);
        cmd.env("CADENCE_GRANT_TOKEN", &token);
        let child = crate::reaper::spawn(&mut cmd)
            .map_err(|e| Error::rejected(format!("running the approved command failed: {e}")))?;
        let pid = child.id();
        self.perm_exec
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                token.clone(),
                super::identity::GrantExec {
                    pid,
                    argv: argv.to_vec(),
                },
            );
        let out = child.wait_with_output();
        self.perm_exec
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&token);
        let out = out.map_err(|e| Error::rejected(format!("the approved command failed: {e}")))?;
        Ok(json!({
            "code": out.status.code().unwrap_or(1),
            "stdout": clip(&out.stdout),
            "stderr": clip(&out.stderr),
        }))
    }
}

/// Drop the master's denied credentials, and the alias, from a child
/// that runs an approved command. The grant token is set by the caller
/// after this, so a readonly tool never receives one.
fn scrub_grant_env(cmd: &mut std::process::Command) {
    cmd.env_remove("CADENCE_ALIAS");
    cmd.env_remove("CADENCE_GRANT_TOKEN");
    for name in crate::master::DENIED_ENV {
        cmd.env_remove(*name);
    }
}

/// Audit stream for permission requests, decisions and uses. The colon
/// keeps it off the agent-id namespace, same as `audit:approvals`.
const PERM_AUDIT_STREAM: &str = "audit:master-permissions";

/// The message id a routed report is queued under — one per report file.
pub(super) fn route_id(project: &str, id: &str, row: &Value) -> String {
    let name = row["name"].as_str().unwrap_or_default();
    let what = if row["kind"] == "question" {
        "question"
    } else {
        "report"
    };
    let path = format!("{project}/{id}/{}/{name}", task_report::DIR);
    format!("{what}-{}", short_hash(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_reads_provider_model_effort_and_fallbacks() {
        let md = "---\nname: master\npreferred: {provider: claude, model: opus, effort: high}\n\
                  fallbacks: [{provider: codex, model: gpt-5.5, effort: high}]\n---\n# x\n";
        let got = preferred(md);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "claude");
        assert_eq!(got[0].1.as_deref(), Some("opus"));
        assert_eq!(got[1].0, "codex");
        assert!(preferred("no frontmatter").is_empty());
    }

    /// CAD-322 round 2 (I2): a bare `master start` resolves
    /// `preferred.provider`, then `fallbacks`, then the daemon's first
    /// accepted provider — an explicit `--provider` wins over all of it.
    #[test]
    fn resolve_provider_honours_preferred_then_falls_back() {
        let prefs = |md: &str| preferred(md);
        // preferred.provider is used when the daemon accepts it.
        let md = "---\npreferred: {provider: pi, model: k, effort: high}\n---\n";
        assert_eq!(resolve_provider(None, &prefs(md)), "pi");
        // An unusable preferred (not in PROVIDERS) skips to fallbacks,
        // then to the first accepted provider.
        let md = "---\npreferred: {provider: codex}\nfallbacks: [{provider: pi}, {provider: cursor}]\n---\n";
        assert_eq!(resolve_provider(None, &prefs(md)), "pi");
        let md = "---\npreferred: {provider: codex}\n---\n";
        assert_eq!(
            resolve_provider(None, &prefs(md)),
            master::PROVIDERS[0],
            "no usable configured provider → first accepted"
        );
        assert_eq!(resolve_provider(None, &prefs("no frontmatter")), "claude");
        // An explicit request wins over a different preferred provider.
        assert_eq!(
            resolve_provider(
                Some("claude"),
                &prefs("---\npreferred: {provider: pi}\n---\n")
            ),
            "claude"
        );
    }

    #[test]
    fn clip_keeps_char_boundaries() {
        let s = "é".repeat(10);
        let c = clip(&s, 5);
        assert!(c.starts_with("éé"), "{c}");
        assert!(c.contains("truncated"));
        assert_eq!(clip("short", 10), "short");
    }

    /// Every method name in `Shared::dispatch_method`'s match — parsed from the
    /// source so the test sees methods added later.
    pub(crate) fn dispatch_methods() -> Vec<String> {
        let src = include_str!("../daemon.rs");
        let start = src
            .find("    fn dispatch_method(\n")
            .expect("dispatch_method fn");
        let body = &src[start..];
        let body = &body[body.find("match method {").expect("match")..];
        let body = &body[..body
            .find("other => Err(Error::rejected(format!(\"Unknown method")
            .expect("end")];
        let mut out = Vec::new();
        for line in body.lines() {
            let t = line.trim_start();
            if line.len() - t.len() != 12 || !t.starts_with('"') {
                continue;
            }
            let Some((arms, _)) = t.split_once("=>") else {
                continue;
            };
            for arm in arms.split('|') {
                let name = arm.trim().trim_matches('"');
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    out.push(name.to_string());
                }
            }
        }
        out
    }

    /// Review round 1, C2: the master policy is an allowlist over the
    /// daemon's method table — every method of `Shared::dispatch` not
    /// in [`MASTER_ALLOWED`] is refused, so a method added later is
    /// refused by default; and nothing that acts for the operator,
    /// messages an agent of its choosing or execs is on the list
    /// (`answer_route` reaches only the author of the question the
    /// master's own answer names, CAD-447).
    #[test]
    fn master_policy_allowlists_the_method_table() {
        let table = dispatch_methods();
        assert!(table.len() > 50, "method table parse: {table:?}");
        for allowed in MASTER_ALLOWED {
            assert!(
                table.contains(&allowed.to_string()),
                "{allowed} not a method"
            );
        }
        for m in &table {
            let open = master_may_call(m);
            assert_eq!(open, MASTER_ALLOWED.contains(&m.as_str()), "{m}");
        }
        for never in [
            "shutdown",
            "agent_send",
            "agent_ask",
            "agent_stop",
            "thread_send",
            "plan_approve",
            "plan_reject",
            "slot_acquire",
            "slot_release",
            "slot_runner",
            "slot_launch",
            "agent_register",
            "agent_set",
            "agent_respond",
            "approval_record",
            "task_accept",
            "agent_file_write",
            "master_start",
            "master_models",
            "reports_changed",
            "memory_finalize",
            "model_defaults_set",
        ] {
            assert!(table.contains(&never.to_string()), "{never}");
            assert!(!master_may_call(never), "{never} must be refused");
        }
        assert!(!master_may_call("a_method_added_tomorrow"));
    }
}
