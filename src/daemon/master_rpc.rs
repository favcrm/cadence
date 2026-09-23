//! The master agent's daemon side (CAD-339): starting it from its agent
//! files, the operator-only agent-file writer, the master policy every
//! RPC passes through, the report router that brings workers' reports
//! and unanswered questions into the master's thread, and the "since
//! you left" summary.
//!
//! The master is the agent whose alias is [`crate::master::ALIAS`]; the
//! daemon recognises it on a connection only through the one identity
//! verifier ([`Shared::caller_identity`], CAD-381) — never from request
//! fields or `CADENCE_ALIAS`. Every check here is a process guard, not
//! a security boundary: a same-uid process that escapes the master's
//! process tree (setsid + double fork) is not recognised as the master,
//! the residual CAD-276 documents for operator authority.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{optional_str, required_str, Caller, Shared, DAEMON_ALIAS};
use crate::client;
use crate::error::{Error, Result};
use crate::issue::{self, task_report};
use crate::master::{self, ALIAS};
use crate::store;

/// Daemon methods the master is refused outright, with the reason it
/// hears. Everything that decides, accepts, merges, reconfigures an
/// agent or answers for the operator is here; reading, proposing a plan
/// and dispatching approved tickets are not.
const MASTER_REFUSED: &[(&str, &str)] = &[
    (
        "plan_approve",
        "approving a plan is the operator's decision",
    ),
    ("plan_reject", "rejecting a plan is the operator's decision"),
    ("approval_record", "merge approvals are the operator's"),
    ("approval_revoke", "merge approvals are the operator's"),
    ("task_verdict", "the master never reviews or accepts work"),
    ("task_accept", "the master never reviews or accepts work"),
    ("agent_file_write", "only the operator changes agent files"),
    ("master_start", "only the operator starts the master"),
    (
        "agent_register",
        "the master works with registered agents; staffing from agent files is CAD-338",
    ),
    (
        "agent_set",
        "the master never changes an agent's launch params",
    ),
    ("agent_remove", "the master never removes agents"),
    ("agent_gc", "the master never removes agents"),
    (
        "agent_unfence",
        "reconciling a fenced agent is the operator's",
    ),
    (
        "agent_respond",
        "answering a provider's permission prompt is the operator's consent",
    ),
    (
        "agent_answer",
        "answering a provider's permission prompt is the operator's consent",
    ),
    (
        "job_new",
        "the master dispatches with `cadence dispatch <ID>`, not jobs",
    ),
    (
        "task_new",
        "the master dispatches with `cadence dispatch <ID>`, not jobs",
    ),
    (
        "task_dispatch",
        "the master dispatches with `cadence dispatch <ID>`, not jobs",
    ),
    (
        "task_reopen",
        "the master dispatches with `cadence dispatch <ID>`, not jobs",
    ),
    ("monitor_register", "the master runs no monitors"),
    ("monitor_dispatch", "the master runs no monitors"),
    ("slot_launch", "the master runs no builds or recipes"),
];

/// Methods whose sends the master may make only as the kickoff of an
/// approved plan ticket.
const MASTER_SENDS: &[&str] = &["agent_send", "agent_ask"];

/// The report router's full-scan period; a `reports_changed` ping (sent
/// by `cadence report file`) scans at once.
const ROUTER_EVERY: Duration = Duration::from_secs(30);
const ROUTER_TICK: Duration = Duration::from_millis(250);
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

impl Shared {
    /// Is a master registered at all — every master check is skipped,
    /// at no cost, on an install without one.
    fn master_exists(&self) -> bool {
        self.store.agent_opt(ALIAS).ok().flatten().is_some()
    }

    /// Is this connection the master's (its process tree, per the one
    /// identity verifier). An underivable identity is not the master —
    /// the operator-only verbs refuse it on their own.
    pub(super) fn caller_is_master(&self, peer_pid: u32) -> bool {
        self.master_exists()
            && matches!(
                self.caller_identity(peer_pid),
                Ok(Caller::Agent(v)) if master::is_master(&v.agent.alias)
            )
    }

    /// CAD-339: the master policy, run before every RPC. The master is
    /// refused [`MASTER_REFUSED`] outright, and may send a message only
    /// as the kickoff of a ticket of an approved plan (`issue` names
    /// it; `cadence dispatch` passes it). A refusal happens before the
    /// method runs, so it leaves no write.
    pub(super) fn master_policy(&self, method: &str, params: &Value, peer_pid: u32) -> Result<()> {
        let refused = MASTER_REFUSED.iter().find(|(m, _)| *m == method);
        let send = MASTER_SENDS.contains(&method);
        if refused.is_none() && !send {
            return Ok(());
        }
        if !self.caller_is_master(peer_pid) {
            return Ok(());
        }
        if let Some((_, why)) = refused {
            return Err(Error::invalid(
                "master_refused",
                format!("the master may not call {method}: {why}"),
            ));
        }
        let Some(issue) = optional_str(params, "issue") else {
            return Err(Error::invalid(
                "master_outside_plan",
                "the master sends only the kickoff of an approved plan ticket — \
                 `cadence dispatch <ID> --to <alias> --reply-to master`",
            ));
        };
        crate::issue::plan::gate_master_id(&self.pm_dir()?, issue)
    }

    /// `plan_check` — the dispatch pre-flight `cadence dispatch` runs
    /// before it writes anything: the CAD-360 gate for everyone, and for
    /// the master the stricter "approved plan tickets only".
    pub(super) fn rpc_plan_check(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let issue = required_str(params, "issue")?;
        let pm_dir = self.pm_dir()?;
        let is_master = self.caller_is_master(peer_pid);
        if is_master {
            crate::issue::plan::gate_master_id(&pm_dir, issue)?;
        } else {
            crate::issue::plan::gate_id(&pm_dir, issue)?;
        }
        Ok(json!({"issue": issue, "ok": true, "master": is_master}))
    }

    /// `agent_file_write` — the one writer of an agent's SOUL.md and
    /// AGENT.md, for the proven operator only (the connection-bound rule
    /// of the approval verbs). The master's digest is recorded so a
    /// later edit around this writer is caught at `master start`.
    pub(super) fn rpc_agent_file_write(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.approval_operator("agent file write", params, peer_pid)?;
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
    /// one `master` agent (managed Claude or Codex, cwd the tracker) and
    /// queues its bootstrap: the briefing built from SOUL.md + AGENT.md.
    /// Its thread starts here, so reports routed to it land in the chat.
    pub(super) fn rpc_master_start(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.approval_operator("master start", params, peer_pid)?;
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
        let installed = master::install_defaults(&pm, "operator")?;
        let files = master::read_files(&pm.dir, ALIAS, true)?;
        master::verify(&self.state_dir, ALIAS, &files)?;
        let agent_md = files
            .iter()
            .find(|(n, _)| n == "AGENT.md")
            .map(|(_, t)| t.as_str())
            .unwrap_or_default();
        let choices = preferred(agent_md);
        let provider = optional_str(params, "provider")
            .map(str::to_string)
            .or_else(|| choices.first().map(|c| c.0.clone()))
            .unwrap_or_else(|| "claude".to_string());
        let endpoint_kind = match provider.as_str() {
            "claude" | "codex" => "managed",
            other => {
                return Err(Error::rejected(format!(
                    "the master runs on claude or codex, not '{other}' (pi comes later)"
                )))
            }
        };
        let choice = choices.iter().find(|c| c.0 == provider);
        let model = optional_str(params, "model")
            .map(str::to_string)
            .or_else(|| choice.and_then(|c| c.1.clone()));
        let effort = optional_str(params, "effort")
            .map(str::to_string)
            .or_else(|| choice.and_then(|c| c.2.clone()));
        let mut launch = serde_json::Map::new();
        if let Some(m) = &model {
            launch.insert("model".into(), json!(m));
        }
        if let Some(e) = &effort {
            launch.insert("effort".into(), json!(e));
        }
        let launch = Value::Object(launch);
        crate::adapter::registry::validate_launch_params(&provider, endpoint_kind, &launch)?;
        let briefing = master::compose(&files);
        let file = client::briefing_path(&self.state_dir, &Value::Null, ALIAS);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&file, &briefing)?;
        let cwd = pm.dir.canonicalize()?;
        self.store.register_agent(&store::NewAgent {
            alias: ALIAS,
            provider: &provider,
            endpoint_kind,
            role: "worker",
            cwd: &cwd.to_string_lossy(),
            // Codex writes only inside the tracker; Claude's tools are
            // restricted by the adapter instead.
            sandbox: "workspace-write",
            instructions: Some(&briefing),
            params: Some(&launch.to_string()),
            team_role: None,
            model_policy: None,
        })?;
        let thread = self.store.ensure_thread(ALIAS)?;
        if let Err(e) = self.launch_actor(ALIAS) {
            // No half-started master: the row goes with its launch.
            let _ = self.store.remove_agent(ALIAS, true);
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
                 Read your briefing at {} first. Reply to the operator in this thread.",
                file.display()
            )
        };
        self.send_as(
            &json!({"alias": ALIAS, "text": body, "message": "bootstrap-master",
                    "source": "bootstrap"}),
            &|_| store::Sender::Unattributed,
        )?;
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "master_started",
            json!({"provider": provider, "model": model, "installed": installed}),
        );
        self.wake();
        Ok(json!({
            "alias": ALIAS,
            "provider": provider,
            "endpoint_kind": endpoint_kind,
            "model": model,
            "effort": effort,
            "state": "starting",
            "installed": installed,
            "agent_dir": master::agent_dir(&pm.dir, ALIAS),
            "briefing": file,
            "thread": thread.to_json(),
        }))
    }

    /// `master_summary` — the "since you left" summary (`since`: epoch,
    /// ISO or a look-back like `24h`). `post` appends it to the master's
    /// thread as a system entry; only the operator or the master posts.
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
            if let Ok(Caller::Agent(v)) = self.caller_identity(peer_pid) {
                if !master::is_master(&v.agent.alias) {
                    return Err(Error::rejected(format!(
                        "only the operator or the master posts into the master's thread — \
                         this connection is '{}'",
                        v.agent.alias
                    )));
                }
            }
        }
        let pm = issue::Pm::at(&self.pm_dir()?)?;
        let mut out = issue::summary::since(&pm, since)?;
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

    /// `reports_changed` — a hint that a report was filed; the router
    /// scans at once instead of at its next period.
    pub(super) fn rpc_reports_changed(&self) -> Result<Value> {
        self.reports_dirty.store(true, Ordering::SeqCst);
        Ok(json!({"ok": true}))
    }

    /// The report router's thread: a scan every [`ROUTER_EVERY`], or at
    /// once after a `reports_changed` ping.
    pub(super) fn run_report_router(self: &Arc<Self>) {
        if !self.report_router {
            return;
        }
        let mut next = Instant::now();
        while !self.closing.load(Ordering::SeqCst) {
            if self.reports_dirty.swap(false, Ordering::SeqCst) || Instant::now() >= next {
                if let Err(e) = self.route_reports() {
                    tracing::debug!("report router: {e}");
                }
                next = Instant::now() + ROUTER_EVERY;
            }
            std::thread::sleep(ROUTER_TICK);
        }
    }

    /// One router pass (CAD-339). With a master registered, every task
    /// report filed after the master was created and not by the master:
    ///
    /// - `done` / `blocked` → one message to the master (so it lands in
    ///   the master's thread and the master can dispatch what is next);
    /// - `question` still open, not escalated, and older than
    ///   `[host] question_escalate_after_secs` (default 900 — the window
    ///   a PM has to answer) → one message to the master with the
    ///   question and how to answer or escalate it.
    ///
    /// Each routes once: the message id is derived from the report path
    /// and an existing id is skipped. Reports from before the master
    /// existed are never replayed into it.
    pub(super) fn route_reports(self: &Arc<Self>) -> Result<usize> {
        let Some(master) = self.store.agent_opt(ALIAS)? else {
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
        let baseline = master.created.floor() as i64;
        let now = now_epoch();
        let mut routed = 0;
        for project in issue::project::list(&pm_dir)? {
            let project_dir = pm_dir.join(&project.key);
            let Ok(entries) = std::fs::read_dir(&project_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().to_string();
                let dir = entry.path();
                if !issue::model::valid_id(&id) || !issue::board::is_real_dir(&dir) {
                    continue;
                }
                // Cheap pre-filter: nothing filed since the baseline.
                let fresh = task_report::names(&dir).iter().any(|n| {
                    n.get(..16)
                        .and_then(|p| issue::time::parse_iso(&basic_to_iso(p)))
                        .is_some_and(|t| t >= baseline)
                });
                if !fresh {
                    continue;
                }
                for row in task_report::list(&dir, &id) {
                    if !row["error"].is_null() {
                        continue;
                    }
                    let Some(at) = row["at"].as_str().and_then(issue::time::parse_iso) else {
                        continue;
                    };
                    if at < baseline || row["agent"].as_str() == Some(ALIAS) {
                        continue;
                    }
                    let route = match row["kind"].as_str() {
                        Some("done" | "blocked") => true,
                        Some("question") => {
                            row["open"] == true && row["escalation"].is_null() && now - at >= grace
                        }
                        _ => false,
                    };
                    if route && self.route_one(&project.key, &id, &row)? {
                        routed += 1;
                    }
                }
            }
        }
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
        let what = if kind == "question" {
            "question"
        } else {
            "report"
        };
        let mid = format!("{what}-{}", short_hash(&path));
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
                 settle it: `--kind escalate` with `escalates: {name}` and a one-paragraph \
                 summary — the operator then sees it in Needs-you.",
                row["impact"].as_str().unwrap_or_default()
            )
        } else {
            format!("[report] {id} {kind} by {agent} at {at}\nReport: {path}\n\n{body}")
        };
        self.send_as(
            &json!({"alias": ALIAS, "text": text, "message": mid, "source": "report"}),
            &|_| store::Sender::Unattributed,
        )?;
        let _ = self.store.event_public(
            ALIAS,
            "report_routed",
            json!({"issue": id, "report": name, "kind": kind, "message": mid}),
        );
        Ok(true)
    }
}

/// `20260917T172400Z` (a report file name's prefix) → ISO.
fn basic_to_iso(p: &str) -> String {
    if p.len() < 16 {
        return String::new();
    }
    format!(
        "{}-{}-{}T{}:{}:{}Z",
        &p[0..4],
        &p[4..6],
        &p[6..8],
        &p[9..11],
        &p[11..13],
        &p[13..15]
    )
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
    fn basic_prefix_parses_like_the_report_filename() {
        assert_eq!(basic_to_iso("20260917T172400Z"), "2026-09-17T17:24:00Z");
        assert_eq!(basic_to_iso("short"), "");
    }

    #[test]
    fn clip_keeps_char_boundaries() {
        let s = "é".repeat(10);
        let c = clip(&s, 5);
        assert!(c.starts_with("éé"), "{c}");
        assert!(c.contains("truncated"));
        assert_eq!(clip("short", 10), "short");
    }

    #[test]
    fn every_refused_method_names_a_reason() {
        for (m, why) in MASTER_REFUSED {
            assert!(!m.is_empty() && !why.is_empty());
            assert!(!MASTER_SENDS.contains(m));
        }
        assert!(MASTER_REFUSED.iter().any(|(m, _)| *m == "plan_approve"));
        assert!(!MASTER_REFUSED.iter().any(|(m, _)| *m == "plan_propose"));
    }
}
