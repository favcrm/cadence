//! The PM checkup (CAD-477): on a timer — no person in the loop — the
//! daemon visits every worker holding a running or queued turn and
//! records one outcome: `nudge`, `escalate` or `leave`.
//!
//! - **nudge**: a pty worker whose pane has probed idle — the same
//!   proof `turn_silent_end` requires — while a delivered turn still
//!   waits on its report is prompted ONCE with the exact
//!   `cadence message result` command. The prompt is CAD-468's
//!   reminder: a daemon `sys-nudge-` row keyed
//!   `report-reminder:<message>:<turn>` ([`Self::report_reminder`]),
//!   deduped once per turn however it was first sent.
//! - **escalate**: the lane cannot proceed without a person — a pane
//!   still idle at the NEXT checkup after its reminder retires the
//!   turn `unknown` and fences the agent (one `fenced` Needs-you
//!   row); a fenced agent, or a stopped one auto-resume does not own,
//!   with work waiting; an open question or a blocked report past its
//!   PM grace with no live route to the master — written to the
//!   daemon's escalation record, the only source Needs-you reads.
//! - **leave**: a lane that reported progress (a completed or acked
//!   turn, a busy pane — never pasted into), a prompt still in
//!   flight, or a healthy queue.
//!
//! What the checkup never does: merge, restart a provider, or touch a
//! busy lane. Its writes are the reminder nudge, the guarded `unknown`
//! finish, the escalation record and its own `checkup` events — plus,
//! for a lane that reported done and is idle, CAD-484's one next
//! action ([`super::next_action`]): a fix turn, a review routing, one
//! safe ready-ticket dispatch, or a Needs-you row.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::master_rpc;
use super::next_action;
use super::{format_unknown_fence, Shared, AUTO_STOP_EVENT, AUTO_STOP_MARKER_KINDS, DAEMON_ALIAS};
use crate::error::Result;
use crate::issue::{self, task_report};
use crate::master;
use crate::proto;
use crate::store::{self, Agent, Message};

/// The outcome kind a checkup visit records on the visited worker.
pub(super) const CHECKUP_EVENT: &str = "checkup";

/// Default pass interval: `ServeOptions.checkup` is `None`.
pub(super) const DEFAULT_CHECKUP_SECS: u64 = 60;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Outcome {
    Nudge,
    Escalate,
    Leave,
    /// CAD-484: one fix turn went to the lane's open PR.
    Fix,
    /// CAD-484: the review loop staffed a reviewer on the lane's head.
    Review,
    /// CAD-484: one safe ready ticket went to the free lane.
    Dispatch,
}

/// What a pty pane's live watch proves for the checkup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Pane {
    /// Probed idle past `silent_end_secs` — the prompt is waiting.
    Idle,
    /// A busy screen, an approval menu or a brokered request — the
    /// wait is explained; never pasted into.
    Busy,
    /// No live watch or a disabled probe — no idle proof exists.
    NoProof,
}

impl Outcome {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Nudge => "nudge",
            Self::Escalate => "escalate",
            Self::Leave => "leave",
            Self::Fix => "fix",
            Self::Review => "review",
            Self::Dispatch => "dispatch",
        }
    }
}

impl Shared {
    /// One checkup pass: the report scan first — an open question or
    /// blocked report its PM never picked up escalates to the
    /// operator's Needs-you, and the walk feeds the done map — then
    /// every agent holding a running or queued turn is visited, and
    /// every lane that reported done and is provably idle gets its one
    /// next action (CAD-484, [`super::next_action`]).
    pub(super) fn checkup_tick(self: &Arc<Self>) {
        let done = match self.checkup_reports() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(event = "checkup_reports_failed", error = e.to_string());
                next_action::DoneMap::new()
            }
        };
        let agents = match self.store.agents() {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(event = "checkup_failed", error = e.to_string());
                return;
            }
        };
        for agent in &agents {
            // A mailbox's unread is its function, not a turn —
            // inbox_unread / inbox_stale already row it.
            if agent.endpoint_kind == "inbox" {
                continue;
            }
            if let Err(e) = self.checkup_agent(agent, done.get(&agent.alias)) {
                tracing::warn!(
                    event = "checkup_agent_failed",
                    alias = agent.alias.as_str(),
                    error = e.to_string()
                );
            }
        }
    }

    /// Visit one agent: decide its outcome, act on it, then record the
    /// `checkup` event — one outcome per visited worker per pass.
    fn checkup_agent(
        self: &Arc<Self>,
        agent: &Agent,
        done: Option<&Vec<next_action::DoneRef>>,
    ) -> Result<()> {
        let running = self.store.running_message(&agent.alias)?;
        let queued = self.store.queued_head(&agent.alias)?;
        if running.is_none() && queued.is_none() {
            // CAD-484: no turn in flight — a lane that reported done
            // still gets its one next action.
            if let Some(reports) = done.filter(|r| !r.is_empty()) {
                return self.checkup_idle_lane(agent, reports);
            }
            return Ok(());
        }
        let head = running.as_ref().or(queued.as_ref());
        let (outcome, reason) = self.checkup_decide(agent, running.as_ref());
        let _ = self.store.event_public(
            &agent.alias,
            CHECKUP_EVENT,
            json!({
                "outcome": outcome.as_str(),
                "message": head.map(|m| m.id.as_str()),
                "running": running.is_some(),
                "reason": reason,
            }),
        );
        Ok(())
    }

    /// The outcome for one visited worker — and the act it implies.
    /// Escalate precedes nudge: a lane already past its prompt is the
    /// operator's, not another nudge's.
    fn checkup_decide(
        self: &Arc<Self>,
        agent: &Agent,
        running: Option<&Message>,
    ) -> (Outcome, String) {
        // A fenced agent's work already waits on the operator —
        // `fenced` is its Needs-you row.
        if agent.state == "attention" {
            return (
                Outcome::Escalate,
                "agent fenced with work waiting — reconcile then resume".to_string(),
            );
        }
        // A stopped agent with work waiting: the idle timer's own stop
        // is auto-resume's to restart (leave), every other stop is the
        // operator's (escalate — the `stopped` row).
        if agent.state == "stopped" {
            let auto = self
                .store
                .last_event_of(&agent.alias, AUTO_STOP_MARKER_KINDS)
                .ok()
                .flatten()
                .is_some_and(|m| m.kind == AUTO_STOP_EVENT);
            return if auto {
                (
                    Outcome::Leave,
                    "auto-stopped — the queued-work resume owns it".to_string(),
                )
            } else {
                (
                    Outcome::Escalate,
                    "agent stopped with work waiting — `cadence agent resume`".to_string(),
                )
            };
        }
        let Some(m) = running else {
            return (
                Outcome::Leave,
                "queued — waiting on the actor to claim it".to_string(),
            );
        };
        if !m.awaiting_report() {
            return (Outcome::Leave, "turn in flight".to_string());
        }
        if !agent.enabled {
            return (Outcome::Leave, "agent disabled".to_string());
        }
        let acked = m
            .result
            .as_ref()
            .is_some_and(|r| r.get("ack").is_some_and(|a| !a.is_null()));
        if acked {
            return (
                Outcome::Leave,
                "ack reported — the turn is alive".to_string(),
            );
        }
        if agent.endpoint_kind != "pty" {
            return (
                Outcome::Leave,
                "turn running — a non-pty endpoint reports on its own".to_string(),
            );
        }
        match self.pane_state(agent) {
            // A busy pane is never pasted into.
            Pane::Busy => (Outcome::Leave, "pane busy — turn active".to_string()),
            Pane::NoProof => (
                Outcome::Leave,
                "no live idle proof — nothing to act on".to_string(),
            ),
            Pane::Idle => self.checkup_idle_turn(agent, m),
        }
    }

    /// What the live pane proves right now — the same verdict
    /// `turn_silent_end` needs for `Idle`: `silent_end_secs` of
    /// consecutive idle samples, never while a menu or a brokered
    /// request explains the wait.
    pub(super) fn pane_state(&self, agent: &Agent) -> Pane {
        let budget = self.silent_end_budget(agent);
        if budget == 0 {
            return Pane::NoProof;
        }
        if self
            .pending
            .lock()
            .unwrap()
            .values()
            .any(|req| req.alias == agent.alias)
        {
            return Pane::Busy;
        }
        let Some(ctl) = self
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .get(&agent.alias)
            .map(Arc::clone)
        else {
            return Pane::NoProof;
        };
        let w = ctl.stall.lock().unwrap();
        if w.menu_line.is_some() {
            return Pane::Busy;
        }
        if w.idle_samples >= 3
            && w.idle_since
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(budget))
        {
            Pane::Idle
        } else {
            Pane::Busy
        }
    }

    /// An idle-at-prompt pty turn awaiting its report: prompt it once
    /// (`nudge`); still idle at the next checkup after the prompt
    /// landed, retire it `unknown` (`escalate`). The reminder row is
    /// the durable "prompted" marker — CAD-468's silent-end reminder
    /// and this one share the key, so a turn is prompted once however
    /// the prompt first went out.
    fn checkup_idle_turn(self: &Arc<Self>, agent: &Agent, m: &Message) -> (Outcome, String) {
        let token = m.turn_id.as_deref().unwrap_or("<turn_id>");
        let key = format!("report-reminder:{}:{token}", m.id);
        let id = proto::daemon_message_id(store::NUDGE_SOURCE, &key);
        match self.store.message(&id) {
            Ok(Some(reminder)) => match reminder.state.as_str() {
                // The prompt is still in flight to the pane — the
                // turn is not yet judged.
                "queued" | "submitting" | "running" => (
                    Outcome::Leave,
                    "report prompt in flight — not yet confirmed".to_string(),
                ),
                // Past the prompt and STILL idle — delivered or dead,
                // the turn goes `unknown` and the `fenced` row names
                // it for the operator.
                _ => self.checkup_expire_idle(
                    agent,
                    m,
                    "pane still idle at its prompt after the report reminder — \
                     no report; outcome unknown; not completed, not replayed",
                ),
            },
            // CAD-468's reminder — the same call the silent-end edge
            // makes; the shared `report-reminder:` key dedupes
            // whichever prompts the turn first. It logs a failed
            // enqueue, so the row re-read keeps the recorded outcome
            // honest: no row, no nudge — the next pass retries.
            Ok(None) => {
                self.report_reminder(agent, m);
                match self.store.message(&id) {
                    Ok(Some(_)) => (
                        Outcome::Nudge,
                        "pane idle with a turn still open — report prompt sent".to_string(),
                    ),
                    _ => (
                        Outcome::Leave,
                        "report prompt did not queue — next checkup retries".to_string(),
                    ),
                }
            }
            Err(e) => (
                Outcome::Leave,
                format!("reminder state unreadable — next checkup retries: {e}"),
            ),
        }
    }

    /// Retire an idle-after-prompt turn `unknown` and fence the agent —
    /// the standard uncertain-outcome writes, guarded like
    /// `report_timeout`: a report that lands first wins.
    fn checkup_expire_idle(
        self: &Arc<Self>,
        agent: &Agent,
        m: &Message,
        why: &str,
    ) -> (Outcome, String) {
        match self.store.expire_awaiting_report(&m.id, None, why) {
            Ok(true) => {
                self.notify_routed_target(m, &Value::Null);
                let _ = self.store.set_state_detached(
                    &agent.alias,
                    "attention",
                    Some(&format_unknown_fence(why)),
                );
                let _ = self
                    .store
                    .event_public(&agent.alias, "attention", json!({"reason": why}));
                self.wake();
                (
                    Outcome::Escalate,
                    "turn unknown — still idle after the report prompt".to_string(),
                )
            }
            Ok(false) => (
                Outcome::Leave,
                "a report landed before the checkup wrote".to_string(),
            ),
            Err(e) => (
                Outcome::Leave,
                format!("unknown write failed — next checkup retries: {e}"),
            ),
        }
    }

    /// The report scan: an open question or a blocked report past its
    /// PM grace — and not on a live route to the master — goes to the
    /// operator's Needs-you through the daemon's own escalation
    /// record, the same write `question_escalate` makes. A report file
    /// still cannot put itself in Needs-you; only this record can.
    fn checkup_reports(self: &Arc<Self>) -> Result<next_action::DoneMap> {
        let Ok(pm_dir) = self.pm_dir() else {
            return Ok(next_action::DoneMap::new());
        };
        self.checkup_reports_in(&pm_dir)
    }

    /// The scan over one tracker dir — split from [`Self::pm_dir`]
    /// resolution so a test can point it at its own pm. Returns the
    /// done map alongside the escalation pass: every `done` report by
    /// the agent who filed it, for the idle-lane visit.
    pub(super) fn checkup_reports_in(
        self: &Arc<Self>,
        pm_dir: &std::path::Path,
    ) -> Result<next_action::DoneMap> {
        let mut done: next_action::DoneMap = next_action::DoneMap::new();
        if !pm_dir.join("pm.yaml").is_file() {
            return Ok(done);
        }
        let grace = crate::doctor::host::read_host_overrides(pm_dir)
            .ok()
            .flatten()
            .and_then(|o| o.question_escalate_after_secs)
            .unwrap_or(master_rpc::QUESTION_GRACE_SECS) as i64;
        let escalated = master::escalations(&self.state_dir);
        let now = crate::issue::time::now_epoch();
        for project in issue::project::list(pm_dir)? {
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
                let status = issue::board::find_issue(pm_dir, &id)
                    .map(|i| i.front.status)
                    .unwrap_or_default();
                let rows = task_report::list(&dir, &id);
                for row in &rows {
                    let name = row["name"].as_str().unwrap_or_default();
                    let kind = row["kind"].as_str().unwrap_or_default();
                    // CAD-484: every done report feeds the idle-lane
                    // map before the escalation filters apply.
                    if kind == "done" {
                        if let Some(filer) = row["agent"].as_str().filter(|a| !a.is_empty()) {
                            done.entry(filer.to_string())
                                .or_default()
                                .push(next_action::DoneRef {
                                    issue: id.clone(),
                                    project: project.key.clone(),
                                    name: name.to_string(),
                                    at: row["at"]
                                        .as_str()
                                        .and_then(issue::time::parse_iso)
                                        .unwrap_or(0),
                                    sha: row["sha"].as_str().map(str::to_string),
                                    pr: row["pr"].as_str().map(str::to_string),
                                    status: status.clone(),
                                });
                        }
                        continue;
                    }
                    let open = match kind {
                        "question" => row["open"] == true,
                        "blocked" => task_report::blocked_open(&rows, row, &status),
                        _ => false,
                    };
                    if !open || escalated.contains_key(&format!("{id}/{name}")) {
                        continue;
                    }
                    let Some(at) = row["at"].as_str().and_then(issue::time::parse_iso) else {
                        continue;
                    };
                    if now - at < grace {
                        continue;
                    }
                    // A route to a live master is the lane working —
                    // the checkup backstops only what nobody reads.
                    if let Some(routed) =
                        self.store
                            .message(&master_rpc::route_id(&project.key, &id, row))?
                    {
                        if matches!(routed.state.as_str(), "queued" | "submitting" | "running") {
                            continue;
                        }
                    }
                    if let Err(e) = self.escalate_report(&project.key, &id, kind, name, row) {
                        tracing::warn!(
                            event = "checkup_escalate_failed",
                            report = format!("{id}/{name}"),
                            error = e.to_string()
                        );
                    }
                }
            }
        }
        Ok(done)
    }

    /// Write one report's escalation and record the outcome on the
    /// worker that filed it. The record is keyed `{issue}/{report}` —
    /// a question the master already escalated, or one escalated
    /// between this pass's read and write, is a duplicate the record
    /// refuses, never a second row.
    fn escalate_report(
        self: &Arc<Self>,
        project: &str,
        id: &str,
        kind: &str,
        name: &str,
        row: &Value,
    ) -> Result<()> {
        let key = format!("{id}/{name}");
        let summary = format!(
            "open {kind} report {project}/{id}/{}/{name} unanswered past the PM grace \
             — escalated by the checkup",
            task_report::DIR
        );
        let record = json!({
            "issue": id, "question": name, "kind": kind,
            "summary": summary, "by": "checkup",
            "at": crate::issue::time::iso(crate::issue::time::now_epoch()),
        });
        {
            let _guard = self
                .escalation_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            master::record_escalation(&self.state_dir, &key, record)?;
        }
        let agent = row["agent"].as_str().unwrap_or_default();
        let alias = if self.store.agent_opt(agent)?.is_some() {
            agent
        } else {
            DAEMON_ALIAS
        };
        let _ = self.store.event_public(
            alias,
            CHECKUP_EVENT,
            json!({
                "outcome": Outcome::Escalate.as_str(),
                "report": key,
                "reason": format!("open {kind} report escalated to the operator"),
            }),
        );
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "checkup_escalated",
            json!({"issue": id, "report": name, "kind": kind, "by": "checkup"}),
        );
        self.wake();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Probe;
    use crate::daemon::{AgentCtl, Notify, StallWatch};
    use crate::store::{NewAgent, Take};
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;
    use std::time::Instant;

    fn shared() -> (tempfile::TempDir, Arc<Shared>) {
        let dir = tempfile::TempDir::new().unwrap();
        let shared = Shared::new(dir.path(), &crate::daemon::ServeOptions::default()).unwrap();
        (dir, shared)
    }

    fn worker(shared: &Shared, alias: &str, endpoint_kind: &str, params: Option<&str>) {
        let provider = if endpoint_kind == "pty" {
            "devin"
        } else {
            "fake"
        };
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider,
                endpoint_kind,
                role: "worker",
                cwd: "/tmp",
                sandbox: "read-only",
                instructions: None,
                params,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }

    /// Deliver `id` to `alias` as a pty turn awaiting its report.
    fn running_awaiting(shared: &Shared, alias: &str, id: &str) -> Message {
        shared
            .store
            .enqueue(alias, "do work", None, id, "test")
            .unwrap();
        let Take::Message(m) = shared.store.take_queued(alias).unwrap() else {
            panic!("queued message must be taken");
        };
        shared.store.mark_running(&m.id, "pty-t1").unwrap();
        let m = shared.store.message(&m.id).unwrap().unwrap();
        shared.store.mark_submitted(&m).unwrap();
        let m = shared.store.message(&m.id).unwrap().unwrap();
        assert!(m.awaiting_report(), "{m:?}");
        m
    }

    /// Park `alias`'s stall watch in a provably-idle state — the same
    /// evidence the silent-end edge requires (silent_end_secs=1 set on
    /// the agent, three idle samples, streak older than the budget).
    fn idle_ctl(shared: &Arc<Shared>, alias: &str) {
        ctl_with(
            shared,
            alias,
            StallWatch {
                message: None,
                idle_samples: 4,
                idle_since: Some(Instant::now() - Duration::from_secs(30)),
                last_probe: Some(Probe {
                    idle: true,
                    reason: "idle".into(),
                    input_nonempty: false,
                    prompt_visible: true,
                    busy_marker: false,
                    approval_menu: false,
                }),
                ..StallWatch::default()
            },
        );
    }

    fn busy_ctl(shared: &Arc<Shared>, alias: &str) {
        ctl_with(
            shared,
            alias,
            StallWatch {
                idle_samples: 0,
                idle_since: None,
                last_probe: Some(Probe {
                    idle: false,
                    reason: "busy marker".into(),
                    input_nonempty: false,
                    prompt_visible: false,
                    busy_marker: true,
                    approval_menu: false,
                }),
                ..StallWatch::default()
            },
        );
    }

    fn ctl_with(shared: &Arc<Shared>, alias: &str, watch: StallWatch) {
        let ctl = Arc::new(AgentCtl {
            adapter: Mutex::new(None),
            wake: Notify::new(),
            thread: Mutex::new(None),
            stall: Mutex::new(watch),
            cloud_held: AtomicBool::new(false),
        });
        shared
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .insert(alias.to_string(), ctl);
    }

    fn outcomes(shared: &Shared, alias: &str) -> Vec<String> {
        shared
            .store
            .events_tail(alias, 100)
            .unwrap()
            .iter()
            .filter(|e| e.kind == CHECKUP_EVENT)
            .map(|e| e.payload["outcome"].as_str().unwrap_or("?").to_string())
            .collect()
    }

    fn reminder_row(shared: &Shared, m: &Message) -> Option<Message> {
        let token = m.turn_id.as_deref().unwrap_or("<turn_id>");
        let key = format!("report-reminder:{}:{token}", m.id);
        let id = proto::daemon_message_id(store::NUDGE_SOURCE, &key);
        shared.store.message(&id).unwrap()
    }

    #[test]
    fn visits_each_worker_with_a_turn_and_records_one_outcome() {
        let (_d, s) = shared();
        worker(&s, "w-q", "fake", None);
        worker(&s, "w-r", "fake", None);
        worker(&s, "w-off", "fake", None);
        s.store
            .enqueue("w-q", "queued work", None, "m1", "test")
            .unwrap();
        running_awaiting(&s, "w-r", "m2");

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w-q"), ["leave"]);
        assert_eq!(outcomes(&s, "w-r"), ["leave"]);
        // No running or queued turn — never visited.
        assert_eq!(outcomes(&s, "w-off"), Vec::<String>::new());
        s.checkup_tick();
        assert_eq!(outcomes(&s, "w-q"), ["leave", "leave"]);
    }

    #[test]
    fn idle_unreported_pty_turn_is_prompted_once_then_unknown() {
        let (_d, s) = shared();
        worker(&s, "w1", "pty", Some(r#"{"silent_end_secs": 1}"#));
        let m = running_awaiting(&s, "w1", "m1");
        idle_ctl(&s, "w1");

        // First checkup: the prompt goes out once.
        s.checkup_tick();
        let reminder = reminder_row(&s, &m).expect("a report reminder was queued");
        assert!(
            reminder.body.contains("cadence message result m1"),
            "{}",
            reminder.body
        );
        assert!(
            reminder.body.contains("report_timeout_secs"),
            "{}",
            reminder.body
        );
        assert_eq!(outcomes(&s, "w1"), ["nudge"]);

        // Still queued to the pane: the next checkup leaves — the
        // prompt has not landed, so the turn is not yet judged.
        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["nudge", "leave"]);
        assert_eq!(s.store.message(&m.id).unwrap().unwrap().state, "running");

        // The prompt lands (confirmed paste completes the nudge).
        s.store
            .finish(
                &reminder,
                "completed",
                &json!({"status": "completed"}),
                None,
            )
            .unwrap();
        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["nudge", "leave", "escalate"]);
        let done = s.store.message(&m.id).unwrap().unwrap();
        assert_eq!(done.state, "unknown", "{done:?}");
        assert_eq!(s.store.agent("w1").unwrap().state, "attention");

        // The turn is gone: the fenced lane holds nothing running or
        // queued, so the next pass does not visit it at all — and the
        // reminder row deduped the prompt to once.
        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["nudge", "leave", "escalate"]);
        assert!(reminder_row(&s, &m).is_some());
    }

    #[test]
    fn busy_pane_is_never_prompted() {
        let (_d, s) = shared();
        worker(&s, "w1", "pty", Some(r#"{"silent_end_secs": 1}"#));
        let m = running_awaiting(&s, "w1", "m1");
        busy_ctl(&s, "w1");
        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["leave"]);
        assert!(
            reminder_row(&s, &m).is_none(),
            "a busy pane is never pasted into"
        );
        assert_eq!(s.store.message(&m.id).unwrap().unwrap().state, "running");
    }

    #[test]
    fn acked_and_progressing_lanes_are_left_alone() {
        let (_d, s) = shared();
        worker(&s, "w1", "pty", Some(r#"{"silent_end_secs": 1}"#));
        let m = running_awaiting(&s, "w1", "m1");
        idle_ctl(&s, "w1");
        // The worker acked — explicit activity — so even an idle
        // screen leaves the lane alone.
        s.store.mark_ack(&m, Some("still working")).unwrap();
        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["leave"]);
        assert!(reminder_row(&s, &m).is_none());
    }

    type ReportSpec<'a> = (&'a str, &'a str);
    type IssueSpec<'a> = (&'a str, &'a str, &'a [ReportSpec<'a>]);

    /// A tracker dir with one project and `id` holding the given
    /// report files `(name, frontmatter)`.
    fn pm_with_issues(pm: &std::path::Path, issues: &[IssueSpec<'_>]) {
        let notes = pm.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(
            pm.join("pm.yaml"),
            format!(
                "schema: 1\nnotes_dir: {}\nstatuses:\n- backlog\n- ready\n- doing\n- review\n- done\n- dropped\n",
                notes.display()
            ),
        )
        .unwrap();
        std::fs::create_dir_all(pm.join("tst")).unwrap();
        std::fs::write(pm.join("tst/project.yaml"), "key: tst\nprefix: TST\n").unwrap();
        for (id, owner, reports) in issues {
            let dir = pm.join("tst").join(id);
            std::fs::create_dir_all(dir.join("reports")).unwrap();
            std::fs::write(
                dir.join("issue.md"),
                format!(
                    "---\nid: {id}\ntitle: checkup issue {id}\nstatus: doing\n\
                     priority: P2\nowner: {owner}\ncreated: 2026-01-01T00:00:00Z\n---\n\nbody\n"
                ),
            )
            .unwrap();
            for (name, front) in *reports {
                std::fs::write(dir.join("reports").join(name), front).unwrap();
            }
        }
    }

    fn report_name(at_epoch: i64, agent: &str) -> String {
        // `at_of` parses `YYYYMMDDTHHMMSSZ` — the iso writes
        // `YYYY-MM-DDTHH:MM:SSZ`.
        let iso = crate::issue::time::iso(at_epoch);
        let compact: String = iso.chars().filter(|c| *c != '-' && *c != ':').collect();
        format!("{compact}-{agent}.md")
    }

    /// A stored report file: `front` frontmatter plus the six
    /// reflection headings `load` requires.
    fn report_file(front: &str) -> String {
        format!(
            "---\n{front}---\n\n## Expected\n\nx\n\n## Evidence\n\nx\n\n## Cause\n\nx\n\n\
             ## Correction\n\nx\n\n## Lesson\n\nx\n\n## Next\n\nx\n"
        )
    }

    #[test]
    fn question_and_blocked_past_grace_escalate_to_one_operator_row_each() {
        let (_d, s) = shared();
        worker(&s, "w-ask", "fake", None);
        let pm = tempfile::TempDir::new().unwrap();
        let now = crate::issue::time::now_epoch();
        let qname = report_name(now - 10 * 86_400, "w-ask");
        let bname = report_name(now - 9 * 86_400, "w-ask");
        let fresh = report_name(now, "w-ask");
        pm_with_issues(
            pm.path(),
            &[
                (
                    "TST-1",
                    "w-ask",
                    &[
                        (
                            &qname,
                            &report_file(
                                "schema: cadence.report/2\nkind: question\ntask: TST-1\n\
                                 agent: w-ask\nimpact: blocks the lane\noptions:\n- proceed\n- hold\n",
                            ),
                        ),
                        (
                            &bname,
                            &report_file(
                                "schema: cadence.report/2\nkind: blocked\ntask: TST-1\n\
                                 agent: w-ask\n",
                            ),
                        ),
                    ],
                ),
                // An open question inside the PM grace is the PM's —
                // the checkup does not jump it.
                (
                    "TST-2",
                    "w-ask",
                    &[(
                        &fresh,
                        &report_file(
                            "schema: cadence.report/2\nkind: question\ntask: TST-2\n\
                             agent: w-ask\nimpact: blocks the lane\noptions:\n- proceed\n- hold\n",
                        ),
                    )],
                ),
            ],
        );

        s.checkup_reports_in(pm.path()).unwrap();
        let escalated = crate::master::escalations(&s.state_dir);
        assert_eq!(
            escalated.keys().collect::<std::collections::BTreeSet<_>>(),
            [format!("TST-1/{qname}"), format!("TST-1/{bname}")]
                .iter()
                .collect::<std::collections::BTreeSet<_>>(),
            "{escalated:?}"
        );
        // A second pass records nothing new — each outcome once.
        s.checkup_reports_in(pm.path()).unwrap();
        assert_eq!(crate::master::escalations(&s.state_dir).len(), 2);
        assert_eq!(outcomes(&s, "w-ask"), ["escalate", "escalate"]);

        // Needs-you: exactly one operator row per escalated report.
        let view = crate::overview::overview_cached(&s.state_dir, pm.path());
        let needs = view["needs_me"].as_array().expect("needs_me rows");
        let count = |kind: &str| {
            needs
                .iter()
                .filter(|r| r["kind"].as_str() == Some(kind))
                .count()
        };
        assert_eq!(count("question"), 1, "{needs:?}");
        assert_eq!(count("blocked"), 1, "{needs:?}");

        // The answer still routes back to the worker who asked.
        let aname = report_name(now - 60, "operator");
        std::fs::write(
            pm.path().join("tst/TST-1/reports").join(&aname),
            report_file(&format!(
                "schema: cadence.report/2\nkind: answer\ntask: TST-1\n\
                 agent: operator\nanswers: {qname}\n"
            )),
        )
        .unwrap();
        let out = s
            .route_answer(pm.path(), "TST-1", &aname, "operator")
            .unwrap();
        assert_eq!(out["sent"], json!(true), "{out}");
        assert_eq!(out["to"], json!("w-ask"));
        let routed = s.store.queued_head("w-ask").unwrap().unwrap();
        assert_eq!(routed.source, "answer");
        assert!(
            routed.body.contains("answered your question"),
            "{}",
            routed.body
        );
    }

    /// The stall watch runs the checkup on its own cadence — a live
    /// daemon thread records an outcome with no person in the loop.
    #[test]
    fn the_watch_runs_the_checkup() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = Shared::new(
            dir.path(),
            &crate::daemon::ServeOptions {
                checkup: Some(1),
                auto_stop: Some(crate::daemon::AutoStopSetting::off()),
                report_router: Some(0),
                ..crate::daemon::ServeOptions::default()
            },
        )
        .unwrap();
        worker(&s, "w1", "fake", None);
        s.store.enqueue("w1", "work", None, "m1", "test").unwrap();
        let s2 = Arc::clone(&s);
        let h = std::thread::spawn(move || s2.run_stall_watch());
        let deadline = Instant::now() + Duration::from_secs(15);
        while outcomes(&s, "w1").is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        s.closing.store(true, std::sync::atomic::Ordering::SeqCst);
        h.join().unwrap();
        assert_eq!(outcomes(&s, "w1"), ["leave"]);
    }

    #[test]
    fn stopped_and_fenced_workers_with_work_escalate() {
        let (_d, s) = shared();
        worker(&s, "w-stop", "fake", None);
        worker(&s, "w-auto", "fake", None);
        worker(&s, "w-fence", "fake", None);
        s.store
            .enqueue("w-stop", "work", None, "m1", "test")
            .unwrap();
        s.store
            .enqueue("w-auto", "work", None, "m2", "test")
            .unwrap();
        s.store
            .enqueue("w-fence", "work", None, "m3", "test")
            .unwrap();
        // Manual stop vs the idle timer's own stop.
        s.store.set_agent_state("w-stop", "stopped", None).unwrap();
        s.store.set_agent_state("w-auto", "stopped", None).unwrap();
        s.store
            .event_public("w-auto", AUTO_STOP_EVENT, json!({"idle_secs": 3600.0}))
            .unwrap();
        s.store
            .set_agent_state("w-fence", "attention", None)
            .unwrap();

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w-stop"), ["escalate"]);
        assert_eq!(outcomes(&s, "w-auto"), ["leave"]);
        assert_eq!(outcomes(&s, "w-fence"), ["escalate"]);
        // The checkup restarts nothing and dispatches nothing.
        assert_eq!(s.store.agent("w-stop").unwrap().state, "stopped");
        assert!(s.lifecycle.lock().unwrap().agents.is_empty());
        assert_eq!(s.store.queued_count("w-stop").unwrap(), 1);
    }
}
