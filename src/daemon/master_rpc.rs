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

use super::{optional_str, required_str, Caller, Shared, DAEMON_ALIAS};
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
];

/// Most reports one router pass queues to the master; the rest wait for
/// the next pass and are counted as the routing backlog.
const ROUTES_PER_PASS: usize = 5;
/// Default wait before an unanswered question reaches the master.
const QUESTION_GRACE_SECS: u64 = 900;
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
        let pm = issue::Pm::at(&self.pm_dir()?)?;
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
        let out = issue::dispatch::run(&pm, id, &args, ALIAS, &self.state_dir, Some(ALIAS))?;
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
        let pm = issue::Pm::at(&self.pm_dir()?)?;
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
        let pm = issue::Pm::at(&self.pm_dir()?)?;
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
        let pm = issue::Pm::at(&self.pm_dir()?)?;
        // Files edited around the writer refuse BEFORE anything is
        // installed or registered — a refusal writes nothing.
        master::verify(
            &self.state_dir,
            ALIAS,
            &master::read_files(&pm.dir, ALIAS, false)?,
        )?;
        // Claude only for the MVP (review round 1, C1): a Codex session
        // cannot yet run read-only with its writes through daemon verbs.
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
        // The provider validated above is the one launched — a refused
        // provider never reaches this line, a missing one defaults to
        // the first entry `master::PROVIDERS` accepts.
        let provider = requested.unwrap_or(master::PROVIDERS[0]);
        let choice = preferred(agent_md).into_iter().find(|c| c.0 == provider);
        let model = optional_str(params, "model")
            .map(str::to_string)
            .or_else(|| choice.as_ref().and_then(|c| c.1.clone()));
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
        crate::adapter::registry::validate_launch_params(provider, "managed", &launch)?;
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
            let operator = master::operator_claude_config(
                self.provider_env.var("CLAUDE_CONFIG_DIR"),
                self.provider_env.var("HOME"),
            );
            Some(match operator {
                Some(dir) => master::copy_login(&self.state_dir, &dir)?,
                None => master::ensure_config_dir(&self.state_dir)?,
            })
        } else {
            Some(master::ensure_config_dir(&self.state_dir)?)
        };
        let login_command =
            (login == Some(master::Login::None)).then(|| master::login_command(&self.state_dir));
        self.store.register_agent(&store::NewAgent {
            alias: ALIAS,
            provider,
            endpoint_kind: "managed",
            role: "worker",
            cwd: &cwd.canonicalize()?.to_string_lossy(),
            sandbox: "read-only",
            instructions: Some(&briefing),
            params: Some(&launch.to_string()),
            team_role: None,
            model_policy: None,
        })?;
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
            json!({"provider": provider, "model": model, "installed": installed,
                   "confined": !unconfined,
                   "login": login.map(master::Login::as_str)}),
        );
        if login == Some(master::Login::Copied) {
            let _ = self.store.event_public(
                ALIAS,
                "master_login_copied",
                json!({"by": "operator", "what": "claudeAiOauth",
                       "to": master::claude_config_dir(&self.state_dir)}),
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
        let pm = issue::Pm::at(&self.pm_dir()?)?;
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
}

/// The message id a routed report is queued under — one per report file.
fn route_id(project: &str, id: &str, row: &Value) -> String {
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
