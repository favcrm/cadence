//! CAD-139: idea pipeline router and the operator's decision RPC.
//!
//! The router may message only the project's researcher and architect.
//! It never dispatches a developer, opens a pull request, or creates
//! child tickets. Children are created by `idea_decide`, and only
//! after the plan is at the gate.

use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use super::{optional_str, required_str, Shared, DAEMON_ALIAS};
use crate::error::Result;
use crate::issue::{self, board, idea, project};
use crate::store;

const POLL_STEPS: usize = 6;
const POLL_PAUSE: Duration = Duration::from_millis(40);

impl Shared {
    /// One router wake: notice new ideas, spend at most one research
    /// turn and one plan turn, and stop at `plan_ready`.
    pub(super) fn route_ideas(self: &std::sync::Arc<Self>) -> Result<usize> {
        let _g = self.idea_lock.lock().unwrap_or_else(|e| e.into_inner());
        let pm = match self.pm() {
            Ok(pm) => pm,
            Err(_) => return Ok(0),
        };
        if !pm.dir.is_dir() {
            return Ok(0);
        }
        let mut passes = 0;
        for _ in 0..POLL_STEPS {
            let waiting = self.idea_pass(&pm)?;
            passes += 1;
            if !waiting {
                break;
            }
            thread::sleep(POLL_PAUSE);
        }
        Ok(passes)
    }

    fn idea_pass(self: &std::sync::Arc<Self>, pm: &issue::Pm) -> Result<bool> {
        let mut records = idea::load(&self.state_dir)?;
        let projects = project::list(&pm.dir).unwrap_or_default();
        let mut issues = Vec::new();
        for project in &projects {
            let dir = pm.dir.join(&project.key);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().to_string();
                if !issue::model::valid_id(&id) || !board::is_real_dir(&entry.path()) {
                    continue;
                }
                let Ok((front, body)) = issue::write::load_front(&entry.path()) else {
                    continue;
                };
                if idea::is_idea(&front) {
                    issues.push((project.key.clone(), front, body));
                }
            }
        }
        issues.sort_by(|a, b| a.1.id.cmp(&b.1.id));
        let now = issue::time::now_epoch();
        let mut waiting = false;
        for (project_key, front, body) in issues {
            if self.advance_idea(pm, &mut records, &project_key, &front, &body, now)? {
                waiting = true;
            }
        }
        if !records.is_empty() {
            idea::save(&self.state_dir, &records)?;
        }
        Ok(waiting)
    }

    /// `true` when this idea is waiting on a research or plan turn that
    /// has not finished yet.
    fn advance_idea(
        self: &std::sync::Arc<Self>,
        pm: &issue::Pm,
        records: &mut BTreeMap<String, idea::Record>,
        project_key: &str,
        front: &issue::model::Front,
        body: &str,
        now: i64,
    ) -> Result<bool> {
        let id = front.id.as_str();
        if !records.contains_key(id) {
            if front.status != "backlog" {
                return Ok(false);
            }
            records.insert(
                id.to_string(),
                idea::Record {
                    issue: id.to_string(),
                    project: project_key.to_string(),
                    state: "seen".into(),
                    event_emitted: false,
                    research_message: None,
                    plan_message: None,
                    research_at: None,
                    plan_ready_at: None,
                    stale: false,
                    recommendation: None,
                    tickets: vec![],
                    duplicate_of: None,
                    research_note: None,
                    decision: None,
                },
            );
        }
        let state = {
            let rec = records.get_mut(id).expect("just inserted");
            if !rec.event_emitted {
                let policy = project_policy(pm, project_key);
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "intake_idea",
                    json!({
                        "issue": id,
                        "project": project_key,
                        "auto_research": policy.auto_research,
                    }),
                );
                rec.event_emitted = true;
            }
            rec.state.clone()
        };
        match state.as_str() {
            "seen" => self.start_research(pm, records, project_key, front, body, now),
            "researching" => self.finish_research(pm, records, front, body),
            "planning" => self.finish_plan(pm, records, front, now),
            "plan_ready" => self.mark_stale(pm, records, id, now),
            "parked" => self.reopen_park(pm, records, id, now),
            _ => Ok(false),
        }
    }

    fn start_research(
        self: &std::sync::Arc<Self>,
        pm: &issue::Pm,
        records: &mut BTreeMap<String, idea::Record>,
        project_key: &str,
        front: &issue::model::Front,
        body: &str,
        now: i64,
    ) -> Result<bool> {
        let id = front.id.as_str();
        let policy = project_policy(pm, project_key);
        if !policy.auto_research {
            if let Some(rec) = records.get_mut(id) {
                rec.state = "held".into();
            }
            return Ok(false);
        }
        let open = idea::open_issues_for_dedupe(&pm.dir, id)?;
        let pairs: Vec<(&str, &str)> = open.iter().map(|(i, t)| (i.as_str(), t.as_str())).collect();
        if let Some(other) = idea::near_duplicate(&front.title, &pairs) {
            idea::link_duplicate(pm, id, &other)?;
            idea::comment_once(
                pm,
                id,
                "daemon",
                "dedupe",
                &format!(
                    "Near-duplicate of {other}; linked instead of researching. The operator decides whether to keep this idea."
                ),
            )?;
            if let Some(rec) = records.get_mut(id) {
                rec.state = "duplicate".into();
                rec.duplicate_of = Some(other);
            }
            return Ok(false);
        }
        if idea::cap_reached(records, now, policy.max_per_day) {
            idea::comment_once(
                pm,
                id,
                "daemon",
                "cap",
                &format!(
                    "Idea pipeline daily cap reached ({}); this idea was left untouched.",
                    policy.max_per_day
                ),
            )?;
            if let Some(rec) = records.get_mut(id) {
                rec.state = "capped".into();
            }
            return Ok(false);
        }
        let Some(roles) = idea::team_roles(&pm.dir, project_key) else {
            idea::comment_once(
                pm,
                id,
                "daemon",
                "unstaffed",
                "Idea pipeline stopped: team.yaml has no roles.researcher.alias and roles.architect.alias. No turn was spent.",
            )?;
            if let Some(rec) = records.get_mut(id) {
                rec.state = "unstaffed".into();
            }
            return Ok(false);
        };
        let mid = idea::research_message_id(id);
        let text = idea::research_prompt(id, &front.title, body);
        if let Err(e) = self.send_role(&roles.researcher, &mid, &text) {
            idea::comment_once(
                pm,
                id,
                "daemon",
                "unstaffed",
                &format!(
                    "Idea pipeline stopped before research: {}. No developer was messaged.",
                    e
                ),
            )?;
            if let Some(rec) = records.get_mut(id) {
                rec.state = "unstaffed".into();
            }
            return Ok(false);
        }
        if let Some(rec) = records.get_mut(id) {
            rec.state = "researching".into();
            rec.research_message = Some(mid);
            if rec.research_at.is_none() {
                rec.research_at = Some(now);
            }
        }
        Ok(true)
    }

    fn finish_research(
        self: &std::sync::Arc<Self>,
        pm: &issue::Pm,
        records: &mut BTreeMap<String, idea::Record>,
        front: &issue::model::Front,
        body: &str,
    ) -> Result<bool> {
        let id = front.id.as_str();
        let mid = idea::research_message_id(id);
        let Some(text) = self.turn_text(&mid)? else {
            return Ok(true);
        };
        if text.is_empty() {
            idea::comment_once(
                pm,
                id,
                "daemon",
                "research",
                "Research turn ended with no note. The pipeline stopped; it will not ask again.",
            )?;
            if let Some(rec) = records.get_mut(id) {
                rec.state = "research_failed".into();
            }
            return Ok(false);
        }
        let roles = idea::team_roles(&pm.dir, &records[id].project);
        let author = roles
            .as_ref()
            .map(|r| r.researcher.as_str())
            .unwrap_or("researcher");
        idea::comment_once(
            pm,
            id,
            author,
            "research",
            &format!("Research note:\n\n{text}"),
        )?;
        let Some(roles) = roles else {
            if let Some(rec) = records.get_mut(id) {
                rec.state = "unstaffed".into();
                rec.research_note = Some(text);
            }
            return Ok(false);
        };
        let plan_id = idea::plan_message_id(id);
        let prompt = idea::plan_prompt(id, &front.title, body, &text);
        if let Err(e) = self.send_role(&roles.architect, &plan_id, &prompt) {
            idea::comment_once(
                pm,
                id,
                "daemon",
                "unstaffed",
                &format!("Idea pipeline stopped before the plan turn: {e}"),
            )?;
            if let Some(rec) = records.get_mut(id) {
                rec.state = "unstaffed".into();
                rec.research_note = Some(text);
            }
            return Ok(false);
        }
        if let Some(rec) = records.get_mut(id) {
            rec.state = "planning".into();
            rec.plan_message = Some(plan_id);
            rec.research_note = Some(text);
        }
        Ok(true)
    }

    fn finish_plan(
        self: &std::sync::Arc<Self>,
        pm: &issue::Pm,
        records: &mut BTreeMap<String, idea::Record>,
        front: &issue::model::Front,
        now: i64,
    ) -> Result<bool> {
        let id = front.id.as_str();
        let mid = idea::plan_message_id(id);
        let Some(text) = self.turn_text(&mid)? else {
            return Ok(true);
        };
        let roles = idea::team_roles(&pm.dir, &records[id].project);
        let author = roles
            .as_ref()
            .map(|r| r.architect.as_str())
            .unwrap_or("architect");
        match idea::parse_plan(&text) {
            Ok(plan) => {
                idea::comment_once(pm, id, author, "plan", &text)?;
                idea::set_status_tags(pm, id, "review", Some("plan-ready"), &[])?;
                if let Some(rec) = records.get_mut(id) {
                    rec.state = "plan_ready".into();
                    rec.plan_ready_at = Some(now);
                    rec.recommendation = Some(plan.recommendation);
                    rec.tickets = plan.tickets;
                }
            }
            Err(why) => {
                idea::comment_once(
                    pm,
                    id,
                    "daemon",
                    "plan",
                    &format!(
                        "Plan turn did not meet the gate ({why}). The idea was left as it was; no child tickets were created."
                    ),
                )?;
                if let Some(rec) = records.get_mut(id) {
                    rec.state = "plan_invalid".into();
                }
            }
        }
        Ok(false)
    }

    fn mark_stale(
        &self,
        pm: &issue::Pm,
        records: &mut BTreeMap<String, idea::Record>,
        id: &str,
        now: i64,
    ) -> Result<bool> {
        let ready = records.get(id).and_then(|r| r.plan_ready_at);
        let already = records.get(id).is_some_and(|r| r.stale);
        if already {
            return Ok(false);
        }
        let Some(ready) = ready else {
            return Ok(false);
        };
        if !idea::stale_due(ready, now) {
            return Ok(false);
        }
        idea::comment_once(
            pm,
            id,
            "daemon",
            "stale",
            &format!("{id} has had no operator decision for 14 days; the plan is stale."),
        )?;
        idea::set_status_tags(pm, id, "review", Some("idea-stale"), &[])?;
        if let Some(rec) = records.get_mut(id) {
            rec.stale = true;
        }
        Ok(false)
    }

    fn reopen_park(
        &self,
        pm: &issue::Pm,
        records: &mut BTreeMap<String, idea::Record>,
        id: &str,
        now: i64,
    ) -> Result<bool> {
        let until = records
            .get(id)
            .and_then(|r| r.decision.as_ref())
            .and_then(|d| d.park_until.clone());
        let Some(until) = until else {
            return Ok(false);
        };
        if idea::ymd(now) < until {
            return Ok(false);
        }
        idea::set_status_tags(pm, id, "review", Some("plan-ready"), &["parked"])?;
        if let Some(rec) = records.get_mut(id) {
            rec.state = "plan_ready".into();
            rec.plan_ready_at = Some(now);
            rec.decision = None;
        }
        Ok(false)
    }

    /// Message one role alias. The caller is the daemon, and the alias
    /// is the one `team.yaml` names — never a developer role.
    fn send_role(
        self: &std::sync::Arc<Self>,
        alias: &str,
        message: &str,
        text: &str,
    ) -> Result<()> {
        self.send_as(
            &json!({
                "alias": alias,
                "text": text,
                "message": message,
                "source": "idea",
            }),
            &|_| Ok(store::Sender::Unattributed),
        )?;
        Ok(())
    }

    /// `None` while the turn is still open. `Some` once it is terminal.
    fn turn_text(&self, message: &str) -> Result<Option<String>> {
        let Some(msg) = self.store.message(message)? else {
            return Ok(Some(String::new()));
        };
        match msg.state.as_str() {
            "completed" => Ok(Some(
                msg.result
                    .as_ref()
                    .and_then(|r| r.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )),
            "failed" | "cancelled" | "interrupted" | "unknown" => Ok(Some(String::new())),
            _ => Ok(None),
        }
    }

    /// `idea_decide` — operator only. The decision is an object on the
    /// pipeline record and a daemon event, not a comment.
    pub(super) fn rpc_idea_decide(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("idea decide", params, peer_pid)?;
        let _g = self.idea_lock.lock().unwrap_or_else(|e| e.into_inner());
        let issue = required_str(params, "issue")?;
        issue::model::check_id(issue)?;
        let action = required_str(params, "action")?;
        let pm = self.pm()?;
        let mut records = idea::load(&self.state_dir)?;
        let out = idea::decide(
            &pm,
            &mut records,
            issue,
            action,
            optional_str(params, "reason"),
            optional_str(params, "park_until"),
        )?;
        idea::save(&self.state_dir, &records)?;
        let _ = self
            .store
            .event_public(DAEMON_ALIAS, "idea_decision", out["decision"].clone());
        Ok(out)
    }
}

fn project_policy(pm: &issue::Pm, key: &str) -> project::IntakePolicy {
    project::load(&pm.dir.join(key).join("project.yaml"))
        .map(|p| p.intake_policy())
        .unwrap_or_default()
}
