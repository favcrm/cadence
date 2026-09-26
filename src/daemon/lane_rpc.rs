//! CAD-608: the issue page's lane. The card reads the issue's own open
//! worktree and the agent its latest open `message` ref names — never a
//! caller-supplied alias. Ask, instruct, interrupt, stop and unfence
//! relay the existing operator routes onto that agent. Reassign stops
//! it, joins a new provider into the same worktree, and dispatches
//! again with a continuity note (`issue::dispatch::run`, the same path
//! `cadence dispatch` uses).
//!
//! Every mutation is operator-only by the connection. `alias` on
//! reassign is the worker to join, so it is lifted out of the
//! operator-field refusal; a `from` that is not this issue's lane is
//! refused before anything is stopped.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{reject_identity_fields, required_str, Shared};
use crate::error::{Error, Result};
use crate::issue::{self, claim};
use crate::peer::AgentCaller;

const VERB: &str = "lane";

/// Fields a reassign may carry. `alias` is the new worker; `from`, when
/// sent, must be this issue's lane agent or the call is cross-issue.
const REASSIGN_FIELDS: &[&str] = &[
    "issue", "provider", "model", "effort", "alias", "note", "from",
];

/// Ask / instruct / interrupt / stop / unfence. `from` is the same
/// cross-issue check. Unfence adds `status`, `resume` and `note`.
const ACT_FIELDS: &[&str] = &["issue", "text", "from", "status", "resume", "note", "wait"];

/// What the card classifies. Inputs are the agent's own row, not a
/// caller story.
pub(crate) struct LaneFacts<'a> {
    pub issue_status: &'a str,
    pub agent_state: &'a str,
    pub unknown: i64,
    pub running: i64,
    pub queued: i64,
    pub quota: &'a Value,
    pub usage_limit: &'a Value,
    pub error: &'a str,
}

/// Busy / idle / fenced / rate-limited / quota, plus `shipped` once the
/// issue itself is done. Fenced wins over quota; quota wins over a
/// rate limit; either wins over busy.
pub(crate) fn classify_lane(f: &LaneFacts<'_>) -> &'static str {
    if f.issue_status == "done" {
        return "shipped";
    }
    if f.unknown > 0 || f.agent_state == "attention" {
        return "fenced";
    }
    let pressure = pressure_label(f.quota, f.usage_limit, f.error);
    if let Some(label) = pressure {
        return label;
    }
    if f.running > 0 || f.queued > 0 || f.agent_state == "busy" {
        return "busy";
    }
    "idle"
}

fn pressure_label(quota: &Value, usage: &Value, error: &str) -> Option<&'static str> {
    for blob in [quota, usage] {
        if blob.is_null() {
            continue;
        }
        let state = blob
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let reason = blob
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let remaining = blob.get("remaining").and_then(Value::as_i64);
        let text = format!("{state} {reason}");
        if text.contains("rate") {
            return Some("rate-limited");
        }
        if remaining == Some(0)
            || state == "exhausted"
            || state == "quota"
            || text.contains("quota")
        {
            return Some("quota");
        }
    }
    let err = error.to_ascii_lowercase();
    if err.contains("rate") && err.contains("limit") {
        return Some("rate-limited");
    }
    if err.contains("quota") {
        return Some("quota");
    }
    None
}

/// Cost chip for the card and the reassign picker. Cursor's own models
/// are the plan; `swe-2*` is the free Devin quota; OpenRouter is paid.
pub(crate) fn cost_label(provider: &str, model: &str) -> &'static str {
    let model = model.to_ascii_lowercase();
    if provider == "openrouter" || model.starts_with("openrouter/") {
        "Paid"
    } else if model.contains("swe-2") {
        "Free (quota)"
    } else if provider == "cursor" {
        "Cursor plan"
    } else if model.is_empty() {
        ""
    } else {
        "Paid"
    }
}

fn text_field<'a>(params: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(Error::rejected(format!("{VERB}: `{key}` must be a string"))),
    }
}

fn known_fields(params: &Value, allow: &[&str], what: &str) -> Result<()> {
    if let Some(key) = params
        .as_object()
        .and_then(|o| o.keys().find(|k| !allow.contains(&k.as_str())))
    {
        return Err(Error::rejected(format!("{what}: unknown field '{key}'")));
    }
    Ok(())
}

/// Operator connection, with `alias` lifted out (it names the worker to
/// join, not the caller). Every other identity-shaped field refuses.
fn admit(shared: &Shared, params: &Value, peer_pid: u32, allow: &[&str], what: &str) -> Result<()> {
    if !params.is_object() {
        return Err(Error::rejected(format!(
            "{what}: params must be a JSON object"
        )));
    }
    let mut gated = params.clone();
    if let Some(obj) = gated.as_object_mut() {
        obj.remove("alias");
    }
    shared.operator_connection(what, &gated, peer_pid)?;
    reject_identity_fields(&gated, what)?;
    known_fields(params, allow, what)
}

fn clean_line(what: &str, text: &str, max: usize) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::rejected(format!("{what} is empty")));
    }
    if text.len() > max || text.chars().any(char::is_control) {
        return Err(Error::rejected(format!(
            "{what} must be one line of at most {max} bytes"
        )));
    }
    Ok(text.to_string())
}

struct Resolved {
    id: String,
    agent: String,
    worktree: PathBuf,
    group: String,
    front_status: String,
}

impl Shared {
    fn load_issue(&self, id: &str) -> Result<(crate::issue::Pm, issue::model::Front, String)> {
        let pm = self.pm()?;
        let (_project, dir) = issue::write::issue_dir(&pm, id)?;
        let (front, body) = issue::write::load_front(&dir)?;
        Ok((pm, front, body))
    }

    /// The issue's lane agent: the newest open message ref that names
    /// one. Several open worktrees are an error — picking would be a guess.
    fn resolve_lane(&self, front: &issue::model::Front) -> Result<Option<Resolved>> {
        let trees = issue::start::open_worktrees(front);
        let worktree = match trees.len() {
            0 => return Ok(None),
            1 => trees.into_iter().next().unwrap(),
            n => {
                return Err(Error::rejected(format!(
                    "{} has {n} open worktrees — lane actions need exactly one",
                    front.id
                )))
            }
        };
        let agent = front.refs.iter().rev().find_map(|r| {
            (r.kind == "message" && r.closed != Some(true))
                .then(|| r.agent.clone())
                .flatten()
        });
        let Some(agent) = agent else {
            return Ok(None);
        };
        let row = self.store.agent_opt(&agent)?.ok_or_else(|| {
            Error::rejected(format!(
                "{}'s lane agent '{agent}' is not registered",
                front.id
            ))
        })?;
        let group = row
            .params
            .as_ref()
            .and_then(|p| p.get("upstream"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| front.claim.as_ref().map(|c| c.by.clone()))
            .ok_or_else(|| {
                Error::rejected(format!(
                    "{}'s lane agent '{agent}' has no group — join it under a PM before acting",
                    front.id
                ))
            })?;
        Ok(Some(Resolved {
            id: front.id.clone(),
            agent,
            worktree,
            group,
            front_status: front.status.clone(),
        }))
    }

    /// `from`, when the caller sent it, must be this issue's lane.
    /// Anything else is an attempt to act on another issue's agent.
    fn same_lane(&self, lane: &Resolved, from: Option<&str>) -> Result<()> {
        if let Some(from) = from {
            if from != lane.agent {
                return Err(Error::rejected(format!(
                    "lane: '{from}' is not {}'s lane ('{}') — refusing to act on another issue's agent",
                    lane.id, lane.agent
                )));
            }
        }
        Ok(())
    }

    fn require_lane(&self, front: &issue::model::Front) -> Result<Resolved> {
        self.resolve_lane(front)?.ok_or_else(|| {
            Error::rejected(format!(
                "{} has no lane — kick off a worker before asking, instructing, or reassigning",
                front.id
            ))
        })
    }

    fn launch_policy(
        &self,
        provider: &str,
        kind: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<()> {
        let spec = crate::adapter::registry::spec(provider, kind)?;
        if let Some(model) = model {
            if !spec.launch_params.contains(&"model") {
                return Err(Error::rejected(format!(
                    "lane reassign: provider '{provider}' does not take a model"
                )));
            }
            crate::model_defaults::validate_model_id(model)?;
            if provider == "pi" {
                let policy = match self.pm_dir() {
                    Ok(dir) => crate::pi_policy::read(&dir)?,
                    Err(_) => None,
                };
                crate::pi_policy::require_allowed(policy.as_ref(), "worker", model)?;
            }
        }
        if let Some(effort) = effort {
            if !spec.launch_params.contains(&"effort") {
                return Err(Error::rejected(format!(
                    "lane reassign: provider '{provider}' does not take an effort"
                )));
            }
            match provider {
                "pi" => crate::adapter::registry::pi_effort(effort)?,
                "claude" => crate::adapter::registry::claude_effort(effort)?,
                "codex" => crate::adapter::registry::codex_effort(effort)?,
                other => {
                    return Err(Error::rejected(format!(
                        "lane reassign: provider '{other}' has no effort vocabulary"
                    )))
                }
            }
        }
        Ok(())
    }

    fn provider_catalog(&self) -> Result<Vec<Value>> {
        let policy = match self.pm_dir() {
            Ok(dir) => crate::pi_policy::read(&dir)?,
            Err(_) => None,
        };
        let mut providers = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for spec in crate::adapter::registry::SPECS {
            if !spec.launch_default || spec.internal || seen.contains(&spec.provider) {
                continue;
            }
            seen.push(spec.provider);
            let takes_model = spec.launch_params.contains(&"model");
            let takes_effort = spec.launch_params.contains(&"effort");
            let models = if spec.provider == "pi" {
                let empty: &[String] = &[];
                json!(policy
                    .as_ref()
                    .map(|p| p.models.allow_for("worker"))
                    .unwrap_or(empty))
            } else if takes_model {
                Value::Null
            } else {
                json!([])
            };
            let efforts: &[&str] = if !takes_effort {
                &[]
            } else {
                match spec.provider {
                    "pi" => crate::adapter::registry::PI_EFFORTS,
                    "claude" => crate::adapter::registry::CLAUDE_EFFORTS,
                    "codex" => crate::adapter::registry::CODEX_EFFORTS,
                    _ => &[],
                }
            };
            providers.push(json!({
                "id": spec.provider,
                "model": takes_model,
                "effort": takes_effort,
                "models": models,
                "efforts": efforts,
            }));
        }
        Ok(providers)
    }

    fn lane_json(&self, front: &issue::model::Front, lane: Option<&Resolved>) -> Result<Value> {
        let Some(lane) = lane else {
            return Ok(Value::Null);
        };
        let show = self.store.agent(&lane.agent)?;
        let messages = self.store.messages(&lane.agent)?;
        let unknown = self.store.unknown_messages(&lane.agent)?.len() as i64;
        let running = messages.iter().filter(|m| m.state == "running").count() as i64;
        let queued = self.store.queued_count(&lane.agent)?;
        let agent_json = show.to_json();
        let error = agent_json["error"].as_str().unwrap_or("");
        let quota = agent_json.get("quota").unwrap_or(&Value::Null);
        let usage = agent_json.get("usage_limit").unwrap_or(&Value::Null);
        let state = classify_lane(&LaneFacts {
            issue_status: &lane.front_status,
            agent_state: show.state.as_str(),
            unknown,
            running,
            queued,
            quota,
            usage_limit: usage,
            error,
        });
        let model = agent_json["model_configured"]
            .as_str()
            .or(agent_json["model"].as_str())
            .unwrap_or("");
        let branch = front.refs.iter().rev().find_map(|r| {
            (r.kind == "branch" && r.closed != Some(true))
                .then(|| r.path.clone())
                .flatten()
        });
        let pr = front.refs.iter().rev().find_map(|r| {
            (r.kind == "pr" && r.closed != Some(true)).then(|| {
                json!({
                    "url": r.url,
                    "label": r.label,
                })
            })
        });
        let mut last_at = 0.0_f64;
        let mut last_state = String::new();
        for m in &messages {
            for at in [m.completed, m.started, Some(m.created)]
                .into_iter()
                .flatten()
            {
                if at >= last_at {
                    last_at = at;
                    last_state = m.state.clone();
                }
            }
        }
        let cost = cost_label(&show.provider, model);
        Ok(json!({
            "agent": lane.agent,
            "provider": show.provider,
            "endpoint_kind": show.endpoint_kind,
            "model": if model.is_empty() { Value::Null } else { json!(model) },
            "effort": agent_json["effort"].clone(),
            "cost": if cost.is_empty() { Value::Null } else { json!(cost) },
            "branch": branch,
            "pr": pr.unwrap_or(Value::Null),
            "worktree": lane.worktree,
            "group": lane.group,
            "state": state,
            "fenced": state == "fenced",
            "activity": if last_at > 0.0 {
                json!({"at": last_at, "state": last_state})
            } else {
                Value::Null
            },
        }))
    }

    /// `lane_show` — `{issue}`. A caller-supplied agent is an unknown
    /// field, so the card cannot be pointed at another issue's lane.
    pub(super) fn rpc_lane_show(self: &Arc<Self>, params: &Value) -> Result<Value> {
        known_fields(params, &["issue"], "lane show")?;
        let id = issue::model::check_id(required_str(params, "issue")?)?;
        let (_pm, front, _body) = self.load_issue(&id)?;
        let lane = self.resolve_lane(&front)?;
        Ok(json!({
            "issue": id,
            "lane": self.lane_json(&front, lane.as_ref())?,
            "providers": self.provider_catalog()?,
        }))
    }

    fn act_lane(
        &self,
        params: &Value,
        peer_pid: u32,
        what: &str,
    ) -> Result<(Resolved, issue::model::Front)> {
        admit(self, params, peer_pid, ACT_FIELDS, what)?;
        let id = issue::model::check_id(required_str(params, "issue")?)?;
        let (_pm, front, _body) = self.load_issue(&id)?;
        let lane = self.require_lane(&front)?;
        self.same_lane(&lane, text_field(params, "from")?)?;
        Ok((lane, front))
    }

    fn blocked_for_chat(&self, lane: &Resolved) -> Result<()> {
        let show = self.store.agent(&lane.agent)?;
        let unknown = self.store.unknown_messages(&lane.agent)?.len() as i64;
        let label = classify_lane(&LaneFacts {
            issue_status: &lane.front_status,
            agent_state: show.state.as_str(),
            unknown,
            running: 0,
            queued: 0,
            quota: show.quota.as_ref().unwrap_or(&Value::Null),
            usage_limit: &Value::Null,
            error: show.error.as_deref().unwrap_or(""),
        });
        if matches!(label, "fenced" | "quota" | "rate-limited" | "shipped") {
            return Err(Error::rejected(format!(
                "lane is {label} — unfence or reassign before asking or instructing"
            )));
        }
        Ok(())
    }

    /// `lane_ask` — a light status turn. A live pty pane gets a nudge
    /// (turnless, pasted now); every other endpoint gets one operator
    /// `thread_send`, so the answer lands in the lane thread.
    pub(super) fn rpc_lane_ask(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let (lane, _front) = self.act_lane(params, peer_pid, "lane ask")?;
        self.blocked_for_chat(&lane)?;
        let extra = text_field(params, "text")?
            .map(|t| clean_line("lane ask", t, 500))
            .transpose()?;
        let text = match extra {
            Some(extra) => format!("status? {extra}"),
            None => "status?".to_string(),
        };
        let agent = self.store.agent(&lane.agent)?;
        let live_pty = agent.endpoint_kind == "pty"
            && agent.endpoint.is_some()
            && matches!(agent.state.as_str(), "idle" | "busy")
            && self
                .lifecycle
                .lock()
                .unwrap()
                .agents
                .contains_key(&lane.agent);
        let receipt = if live_pty {
            self.send_with(
                &json!({"alias": lane.agent, "text": text, "nudge": true}),
                &|_| Ok(crate::store::Sender::OperatorChat),
                &|_| Ok(AgentCaller::Operator),
            )?
        } else {
            self.rpc_thread_send(&json!({"alias": lane.agent, "text": text}), peer_pid)?
        };
        Ok(json!({
            "issue": lane.id,
            "agent": lane.agent,
            "kind": "status",
            "receipt": receipt,
        }))
    }

    /// `lane_instruct` — a real operator message on the lane thread.
    pub(super) fn rpc_lane_instruct(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let (lane, _front) = self.act_lane(params, peer_pid, "lane instruct")?;
        self.blocked_for_chat(&lane)?;
        let text = clean_line(
            "lane instruct",
            text_field(params, "text")?.unwrap_or(""),
            4_000,
        )?;
        let receipt =
            self.rpc_thread_send(&json!({"alias": lane.agent, "text": text}), peer_pid)?;
        Ok(json!({
            "issue": lane.id,
            "agent": lane.agent,
            "kind": "instruction",
            "receipt": receipt,
        }))
    }

    /// `lane_interrupt` — the daemon's `interrupt` on this issue's agent.
    pub(super) fn rpc_lane_interrupt(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let (lane, _front) = self.act_lane(params, peer_pid, "lane interrupt")?;
        let mut body = json!({"alias": lane.agent, "wait": 0});
        if let Some(wait) = params.get("wait") {
            body["wait"] = wait.clone();
        }
        let out = self.rpc_interrupt(&body, peer_pid)?;
        Ok(json!({"issue": lane.id, "agent": lane.agent, "interrupt": out}))
    }

    /// `lane_stop` — stop this issue's agent. The lane claim stays.
    pub(super) fn rpc_lane_stop(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let (lane, _front) = self.act_lane(params, peer_pid, "lane stop")?;
        let out = self.rpc_stop(&json!({"alias": lane.agent}))?;
        Ok(json!({"issue": lane.id, "agent": lane.agent, "stop": out}))
    }

    /// `lane_unfence` — explicit reconcile status, none implied.
    pub(super) fn rpc_lane_unfence(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let (lane, _front) = self.act_lane(params, peer_pid, "lane unfence")?;
        let status = text_field(params, "status")?.unwrap_or("");
        if !matches!(status, "interrupted" | "completed" | "failed") {
            return Err(Error::rejected(
                "lane unfence needs a reconcile status — interrupted|completed|failed, none preselected",
            ));
        }
        let resume = params
            .get("resume")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let mut body = json!({"alias": lane.agent, "status": status, "resume": resume});
        if let Some(note) = text_field(params, "note")? {
            body["note"] = json!(clean_line("lane unfence note", note, 500)?);
        }
        let out = self.rpc_unfence(&body, peer_pid)?;
        Ok(json!({"issue": lane.id, "agent": lane.agent, "unfence": out}))
    }

    fn settle_live(
        self: &Arc<Self>,
        front: &issue::model::Front,
        alias: &str,
        peer_pid: u32,
    ) -> Result<()> {
        let messages = self.store.messages(alias)?;
        for r in front
            .refs
            .iter()
            .filter(|r| r.kind == "message" && r.closed != Some(true))
        {
            let Some(mid) = r.path.as_deref() else {
                continue;
            };
            if r.agent.as_deref() != Some(alias) {
                continue;
            }
            let Some(m) = messages.iter().find(|m| m.id == mid) else {
                continue;
            };
            match m.state.as_str() {
                "queued" => {
                    if self
                        .store
                        .cancel(mid, "operator", Some("lane reassign"))
                        .is_err()
                    {
                        self.rpc_interrupt(&json!({"alias": alias, "wait": 2}), peer_pid)?;
                    }
                }
                "submitting" | "running" => {
                    self.rpc_interrupt(&json!({"alias": alias, "wait": 2}), peer_pid)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// `lane_reassign` — `{issue, provider, model?, effort?, alias?, note?, from?}`.
    /// Stops the current lane agent, joins the new one in the same
    /// worktree, and dispatches with a continuity note. The lock makes
    /// a racing pair leave exactly one enabled worker on that worktree.
    pub(super) fn rpc_lane_reassign(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        admit(self, params, peer_pid, REASSIGN_FIELDS, "lane reassign")?;
        let id = issue::model::check_id(required_str(params, "issue")?)?;
        let provider = required_str(params, "provider")?;
        let kind = crate::adapter::registry::default_kind(provider)?;
        let model = text_field(params, "model")?;
        let effort = text_field(params, "effort")?;
        let alias_arg = text_field(params, "alias")?;
        let from = text_field(params, "from")?;
        let note = text_field(params, "note")?
            .map(|n| clean_line("lane reassign note", n, 500))
            .transpose()?;
        if let Some(alias) = alias_arg {
            claim::check_alias(alias, "alias")?;
        }
        // Policy before any stop, so an off-policy model leaves the
        // current lane running.
        self.launch_policy(provider, kind, model, effort)?;

        let _serial = self.dispatch_lock.lock().unwrap_or_else(|e| e.into_inner());
        let pm = self.pm()?;
        let (_project, dir) = issue::write::issue_dir(&pm, &id)?;
        let (front, _body) = issue::write::load_front(&dir)?;
        let lane = self.require_lane(&front)?;
        self.same_lane(&lane, from)?;
        if alias_arg == Some(lane.agent.as_str()) {
            return Err(Error::rejected(
                "lane reassign: the new alias is the current lane agent — pick another",
            ));
        }
        let new_alias = match alias_arg {
            Some(alias) => alias.to_string(),
            None => {
                let mut picked = String::new();
                for _ in 0..8 {
                    let n = &Uuid::new_v4().simple().to_string()[..6];
                    let cand = format!("{provider}-{n}");
                    if self.store.agent_opt(&cand)?.is_none() {
                        picked = cand;
                        break;
                    }
                }
                if picked.is_empty() {
                    return Err(Error::rejected(
                        "lane reassign: could not mint a free worker alias",
                    ));
                }
                picked
            }
        };
        if new_alias == lane.group {
            return Err(Error::rejected(
                "lane reassign: the worker alias cannot be the group itself",
            ));
        }
        if self.store.agent_opt(&new_alias)?.is_some() {
            return Err(Error::rejected(format!(
                "lane reassign: '{new_alias}' is already registered"
            )));
        }

        let spec = crate::adapter::registry::spec(provider, kind)?;
        let mut launch = Map::new();
        launch.insert("upstream".to_string(), json!(lane.group));
        if let Some(model) = model {
            if spec.launch_params.contains(&"model") {
                launch.insert("model".to_string(), json!(model));
            }
        }
        if let Some(effort) = effort {
            if spec.launch_params.contains(&"effort") {
                launch.insert("effort".to_string(), json!(effort));
            }
        }
        let launch_text = Value::Object(launch).to_string();
        let worktree = lane.worktree.to_string_lossy().into_owned();
        self.rpc_register(
            &json!({
                "alias": new_alias,
                "provider": provider,
                "endpoint_kind": kind,
                "role": "worker",
                "cwd": worktree,
                "params": launch_text,
            }),
            peer_pid,
        )?;

        let previous = lane.agent.clone();
        let continuity = match &note {
            Some(note) => format!(
                "Continuity: {id} stays in this worktree. Previous agent was {previous}. {note}"
            ),
            None => {
                format!("Continuity: {id} stays in this worktree. Previous agent was {previous}.")
            }
        };
        let note_path = self.state_dir.join(format!("reassign-{id}-{new_alias}.md"));
        std::fs::write(&note_path, &continuity)
            .map_err(|e| Error::internal(format!("lane reassign note: {e}")))?;

        if let Err(e) = self.settle_live(&front, &previous, peer_pid) {
            let _ = self.rpc_stop(&json!({"alias": new_alias}));
            return Err(e);
        }

        let holders = claim::holders(&front);
        let take_over = if holders.contains(&lane.group.as_str()) {
            None
        } else {
            Some(format!("reassign the lane to {provider}"))
        };
        let args = issue::dispatch::DispatchArgs {
            to: new_alias.clone(),
            note: Some(note_path.clone()),
            name: None,
            base: None,
            repo: None,
            reply_to: Some(lane.group.clone()),
            summary: None,
            job_spec: None,
            no_lessons: false,
            force: false,
            take_over,
        };
        let dispatched = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            issue::dispatch::run(
                &pm,
                &id,
                &args,
                "operator",
                &self.state_dir,
                Some(&lane.group),
            )
        });
        let mut out = match dispatched {
            Ok(out) if out["dispatched"] == true => out,
            Ok(out) => {
                let _ = self.rpc_stop(&json!({"alias": new_alias}));
                return Err(Error::rejected(format!(
                    "lane reassign did not dispatch a new kickoff: {out}"
                )));
            }
            Err(e) => {
                let _ = self.rpc_stop(&json!({"alias": new_alias}));
                return Err(e);
            }
        };
        if let Err(e) = issue::write::add_comment(
            &pm,
            &id,
            &continuity,
            Some("operator"),
            Some("note"),
            None,
            "operator",
        ) {
            out["comment_error"] = json!(e.to_string());
        }
        self.rpc_stop(&json!({"alias": previous}))?;
        out["provider"] = json!(provider);
        out["group"] = json!(lane.group);
        out["previous"] = json!(previous);
        out["continuity"] = json!(continuity);
        out["worktree_kept"] = json!(worktree);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>(status: &'a str, state: &'a str) -> LaneFacts<'a> {
        LaneFacts {
            issue_status: status,
            agent_state: state,
            unknown: 0,
            running: 0,
            queued: 0,
            quota: &Value::Null,
            usage_limit: &Value::Null,
            error: "",
        }
    }

    #[test]
    fn classify_prefers_fence_then_quota_then_busy() {
        assert_eq!(classify_lane(&facts("doing", "idle")), "idle");
        let mut busy = facts("doing", "idle");
        busy.running = 1;
        assert_eq!(classify_lane(&busy), "busy");
        let mut fenced = facts("doing", "idle");
        fenced.unknown = 1;
        fenced.running = 3;
        assert_eq!(classify_lane(&fenced), "fenced");
        let quota = json!({"state": "exhausted", "reason": "quota"});
        let mut q = facts("doing", "busy");
        q.quota = &quota;
        q.running = 1;
        assert_eq!(classify_lane(&q), "quota");
        let limited = json!({"state": "limited", "reason": "rate limit"});
        let mut r = facts("doing", "idle");
        r.usage_limit = &limited;
        assert_eq!(classify_lane(&r), "rate-limited");
        assert_eq!(classify_lane(&facts("done", "busy")), "shipped");
    }

    #[test]
    fn cost_labels_match_the_mockup() {
        assert_eq!(cost_label("cursor", "grok-4.7-high"), "Cursor plan");
        assert_eq!(cost_label("devin", "swe-2-high"), "Free (quota)");
        assert_eq!(cost_label("pi", "devin/swe-2-max"), "Free (quota)");
        assert_eq!(cost_label("openrouter", "openrouter/example-flash"), "Paid");
        assert_eq!(cost_label("pi", "devin/deepseek-v4"), "Paid");
        assert_eq!(cost_label("fake", ""), "");
    }
}
