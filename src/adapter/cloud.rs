//! Devin Cloud adapter: one hosted session over the v3 REST API.
//!
//! `open` creates or adopts a session, `run_turn` posts a message and
//! polls `status_detail` inline, `close` archives (resumable), and
//! `interrupt` terminates. A 429, 5xx, timeout, or transport error
//! during a poll is retried inside the turn budget. Only an exhausted
//! budget returns [`Error::OutcomeUnknown`], and the session stays in
//! place — the actor holds the last status instead of treating the
//! provider as dead. A 429 at create or preflight means nothing was
//! created and is retried, then reported as a provider error rather
//! than an unknown outcome. A create timeout looks up the per-create
//! agent tag (`cadence-agent:<alias>:<nonce>`) before a retry or an
//! unknown report, and will not adopt another agent's session.
//!
//! When `max_acu_limit` is unset, create sends **10**. That is one
//! focused task, not an open-ended session; operators override it with
//! the launch param. No test here talks to `api.devin.ai`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use super::{
    AdapterHooks, Identity, ProviderAdapter, ProviderEnv, ProviderRequest, SettledPoll, TurnResult,
};
use crate::error::{Error, Result};
use crate::store::Agent;

/// Applied at session create when the launch param is absent.
pub const DEFAULT_MAX_ACU_LIMIT: u64 = 10;

const DEFAULT_API_BASE: &str = "https://api.devin.ai";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const LOG_LIMIT: usize = 8192;
const DEFAULT_POLL_INTERVAL_MS: u64 = 1000;
const DEFAULT_POLL_BUDGET_MS: u64 = 30 * 60 * 1000;
const DEFAULT_RECOVER_BUDGET_MS: u64 = 30 * 60 * 1000;

#[derive(Default)]
struct Session {
    id: Option<String>,
    org: String,
    endpoint: Option<String>,
    status: String,
    detail: String,
    acus: Option<f64>,
    limit: Option<f64>,
    wait_sent: bool,
    terminated: bool,
    /// Assistant messages already present before the current post.
    seen: usize,
    message_mark: usize,
    /// Snapshot taken at post time. A terminal or waiting state that
    /// still matches this snapshot is leftover from before the post.
    post_marked: bool,
    post_status: String,
    post_detail: String,
    /// Assistant text that arrived after `message_mark`.
    turn_text: String,
}

enum CallErr {
    Status(u16, String),
    Transport(String),
}

enum Phase {
    Running,
    Wait,
    Done,
    Failed,
    Interrupted,
}

/// Blocking client for one Devin Cloud session.
pub struct DevinCloudAdapter {
    hooks: AdapterHooks,
    env: ProviderEnv,
    http: ureq::Agent,
    base: String,
    interval: Duration,
    budget: Duration,
    recover_budget: Duration,
    /// Exact `cadence-agent:<alias>:<nonce>` tag for the create in flight.
    claim: Mutex<Option<String>>,
    /// Set by stop/interrupt/close so a poll returns instead of blocking.
    release: AtomicBool,
    /// Host checkout path, replaced in prompts so a cloud session is not
    /// told to read a machine-local directory.
    cwd: Mutex<String>,
    session: Mutex<Session>,
    /// Last scrubbed poll failure, included when the budget runs out.
    hold_detail: Mutex<String>,
    log: Mutex<String>,
}

impl DevinCloudAdapter {
    pub fn new(hooks: AdapterHooks, env: &ProviderEnv) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .http_status_as_error(false)
            .build();
        let interval = Duration::from_millis(millis(
            env,
            "CADENCE_DEVIN_POLL_INTERVAL_MS",
            DEFAULT_POLL_INTERVAL_MS,
        ));
        let budget = Duration::from_millis(millis(
            env,
            "CADENCE_DEVIN_POLL_BUDGET_MS",
            DEFAULT_POLL_BUDGET_MS,
        ));
        let recover_budget = Duration::from_millis(millis(
            env,
            "CADENCE_DEVIN_RECOVER_BUDGET_MS",
            DEFAULT_RECOVER_BUDGET_MS,
        ));
        Self {
            hooks,
            env: env.clone(),
            http: ureq::Agent::new_with_config(config),
            base: api_base(env),
            interval,
            budget,
            recover_budget,
            claim: Mutex::new(None),
            release: AtomicBool::new(false),
            cwd: Mutex::new(String::new()),
            session: Mutex::new(Session::default()),
            hold_detail: Mutex::new(String::new()),
            log: Mutex::new(String::new()),
        }
    }

    fn creds(&self) -> Result<(String, String)> {
        let key = configured(&self.env, "CADENCE_DEVIN_API_KEY", "DEVIN_API_KEY");
        let org = configured(&self.env, "CADENCE_DEVIN_ORG_ID", "DEVIN_ORG_ID");
        match (key, org) {
            (Some(key), Some(org)) => Ok((key, org)),
            _ => Err(Error::rejected(
                "devin cloud launch refused: DEVIN_API_KEY and DEVIN_ORG_ID are required",
            )),
        }
    }

    fn turn_text(&self) -> String {
        self.session.lock().unwrap().turn_text.clone()
    }

    fn note(&self, text: &str) {
        let key =
            configured(&self.env, "CADENCE_DEVIN_API_KEY", "DEVIN_API_KEY").unwrap_or_default();
        let mut log = self.log.lock().unwrap();
        log.push_str(&scrub(&key, text.to_string()));
        log.push('\n');
        if log.len() > LOG_LIMIT {
            let mut drop_to = log.len() - LOG_LIMIT;
            while drop_to < log.len() && !log.is_char_boundary(drop_to) {
                drop_to += 1;
            }
            log.drain(..drop_to);
        }
    }

    /// Sleep in short slices so stop can set the release flag without
    /// waiting out a full poll interval. Returns true when released.
    fn pause(&self, delay: Duration) -> bool {
        let step = Duration::from_millis(20);
        let mut left = delay;
        while !left.is_zero() {
            if self.released() {
                return true;
            }
            let slice = left.min(step);
            thread::sleep(slice);
            left = left.saturating_sub(slice);
        }
        self.released()
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> std::result::Result<Value, CallErr> {
        let (key, _) = match self.creds() {
            Ok(c) => c,
            Err(e) => return Err(CallErr::Transport(e.to_string())),
        };
        let url = format!("{}{path}", self.base);
        let auth = format!("Bearer {key}");
        let result = match method {
            "GET" => self.http.get(&url).header("Authorization", &auth).call(),
            "DELETE" => self.http.delete(&url).header("Authorization", &auth).call(),
            "POST" => self
                .http
                .post(&url)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .send_json(body.cloned().unwrap_or_else(|| json!({}))),
            other => {
                return Err(CallErr::Transport(format!("unsupported method {other}")));
            }
        };
        match result {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let raw = match resp.body_mut().read_to_string() {
                    Ok(text) => scrub(&key, text),
                    Err(e) => return Err(CallErr::Transport(scrub(&key, e.to_string()))),
                };
                if !(200..300).contains(&status) {
                    return Err(CallErr::Status(status, bounded(&raw)));
                }
                if raw.trim().is_empty() {
                    return Ok(Value::Null);
                }
                match serde_json::from_str(&raw) {
                    Ok(value) => Ok(value),
                    Err(_) => Ok(json!({"text": bounded(&raw)})),
                }
            }
            Err(e) => Err(CallErr::Transport(scrub(&key, e.to_string()))),
        }
    }

    fn org_path(&self, org: &str, rest: &str) -> String {
        format!("/v3/organizations/{}/sessions{rest}", encode(org))
    }

    fn preflight(&self, org: &str, repos: &[String]) -> Result<()> {
        for repo in repos {
            let (owner, name) = split_repo(repo)?;
            let path = format!(
                "/v3beta1/organizations/{}/repositories?filter_name={}",
                encode(org),
                encode(name)
            );
            match self.get_retry_429(&path) {
                Ok(body) => {
                    if !repo_enabled(&body, owner, name) {
                        return Err(Error::rejected(format!(
                            "repo {repo} not enabled in Devin git connection"
                        )));
                    }
                }
                Err(CallErr::Status(404, _)) => {
                    return Err(Error::rejected(format!(
                        "repo {repo} not enabled in Devin git connection"
                    )));
                }
                Err(CallErr::Status(429, detail)) => {
                    self.note(&detail);
                    return Err(Error::provider(format!(
                        "devin cloud git connection check rate limited for {repo}, nothing created: {detail}"
                    )));
                }
                Err(err) => {
                    let msg = call_text(&err);
                    self.note(&msg);
                    return Err(Error::rejected(format!(
                        "devin cloud could not check git connection for {repo}: {msg}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn create(&self, org: &str, agent: &Agent, repos: &[String]) -> Result<Identity> {
        let limit = acu_limit(agent);
        let mut tags = string_param(agent, "tags");
        if let Some(id) = configured(&self.env, "CADENCE_DAEMON_ID", "CADENCE_DAEMON_ID") {
            let owner = format!("cadence:{id}");
            if !tags.iter().any(|t| t == &owner) {
                tags.push(owner);
            }
        }
        let claim = create_claim(&agent.alias);
        if !tags.iter().any(|tag| tag == &claim) {
            tags.push(claim.clone());
        }
        *self.claim.lock().unwrap() = Some(claim);
        let mut body = Map::new();
        body.insert("prompt".into(), json!(open_prompt(agent)));
        body.insert("max_acu_limit".into(), acu_json(limit));
        body.insert("resumable".into(), json!(true));
        if !repos.is_empty() {
            body.insert("repos".into(), json!(repos));
        }
        if !tags.is_empty() {
            body.insert("tags".into(), json!(tags));
        }
        if let Some(mode) = str_param(agent, "devin_mode") {
            body.insert("devin_mode".into(), json!(mode));
        }
        if let Some(id) = str_param(agent, "playbook_id") {
            body.insert("playbook_id".into(), json!(id));
        }
        if let Some(platform) = str_param(agent, "platform") {
            body.insert("platform".into(), json!(platform));
        }
        for (key, field) in [
            ("knowledge_ids", "knowledge_ids"),
            ("secret_ids", "secret_ids"),
            ("attachment_urls", "attachment_urls"),
        ] {
            let values = string_param(agent, key);
            if !values.is_empty() {
                body.insert(field.into(), json!(values));
            }
        }
        if bool_param(agent, "bypass_approval") {
            body.insert("bypass_approval".into(), json!(true));
        }
        let path = self.org_path(org, "");
        let payload = Value::Object(body);
        let mut delay = Duration::from_millis(40);
        for attempt in 0..4 {
            match self.call("POST", &path, Some(&payload)) {
                Ok(reply) => return self.bind_created(&reply),
                Err(CallErr::Status(429, detail)) => {
                    self.note(&detail);
                    if attempt == 3 {
                        return Err(Error::provider(format!(
                            "devin cloud create rate limited, nothing created: {detail}"
                        )));
                    }
                    if self.pause(delay) {
                        return Err(Error::rejected("devin cloud create interrupted"));
                    }
                    delay = (delay * 2).min(Duration::from_secs(2));
                }
                Err(CallErr::Transport(detail)) => {
                    self.note(&detail);
                    match self.find_owned(org) {
                        Ok(Some(id)) => return self.bind_existing(org, &id),
                        Ok(None) if attempt < 3 => {
                            if self.pause(delay) {
                                return Err(Error::rejected("devin cloud create interrupted"));
                            }
                            delay = (delay * 2).min(Duration::from_secs(2));
                        }
                        Ok(None) => {
                            return Err(Error::provider(
                                "devin cloud create timed out and no owned session was found",
                            ));
                        }
                        Err(err) => return Err(err),
                    }
                }
                Err(CallErr::Status(code, detail)) => {
                    self.note(&detail);
                    return Err(Error::provider(format!(
                        "devin cloud create failed: HTTP {code} {detail}"
                    )));
                }
            }
        }
        Err(Error::provider(
            "devin cloud create rate limited, nothing created",
        ))
    }

    /// GET, retrying 429 inside a short budget. A 429 means the request
    /// did not create a session.
    fn get_retry_429(&self, path: &str) -> std::result::Result<Value, CallErr> {
        let mut delay = Duration::from_millis(40);
        for attempt in 0..4 {
            match self.call("GET", path, None) {
                Err(CallErr::Status(429, detail)) if attempt < 3 => {
                    self.note(&detail);
                    if self.pause(delay) {
                        return Err(CallErr::Transport("interrupted".into()));
                    }
                    delay = (delay * 2).min(Duration::from_secs(2));
                }
                other => return other,
            }
        }
        Err(CallErr::Status(429, "rate limited".into()))
    }

    /// The session created by this attempt, after a timeout that may
    /// still have landed. Matches the per-create agent tag only — a
    /// sibling agent's session on the same daemon is not ours.
    fn find_owned(&self, org: &str) -> Result<Option<String>> {
        let Some(tag) = self.claim.lock().unwrap().clone() else {
            return Ok(None);
        };
        let path = format!(
            "/v3/organizations/{}/sessions?filter_tag={}",
            encode(org),
            encode(&tag)
        );
        match self.call("GET", &path, None) {
            Ok(body) => {
                let sessions = body
                    .get("sessions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for item in sessions {
                    let id = item.get("session_id").and_then(Value::as_str).unwrap_or("");
                    let tagged = item
                        .get("tags")
                        .and_then(Value::as_array)
                        .map(|tags| tags.iter().any(|t| t.as_str() == Some(tag.as_str())))
                        .unwrap_or(false);
                    if tagged && id.starts_with("devin-") {
                        return Ok(Some(id.to_string()));
                    }
                }
                Ok(None)
            }
            Err(err) => {
                let msg = call_text(&err);
                self.note(&msg);
                Err(Error::unknown(format!(
                    "devin cloud create timed out and the owner-tag lookup failed: {msg}"
                )))
            }
        }
    }

    fn bind_created(&self, reply: &Value) -> Result<Identity> {
        let id = reply
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| s.starts_with("devin-"))
            .ok_or_else(|| {
                Error::unknown(
                    "devin cloud create outcome unknown: response had no devin- session id",
                )
            })?
            .to_string();
        let url = session_url(reply, &id);
        self.remember(reply, Some(id.clone()), Some(url.clone()));
        (self.hooks.on_event)(
            "cadence/cloud_session",
            json!({"session_id": id, "endpoint": url, "status": reply.get("status")}),
        );
        Ok(self.identity(&id, url, id.clone()))
    }

    fn bind_existing(&self, org: &str, id: &str) -> Result<Identity> {
        require_session_id(id)?;
        let path = self.org_path(org, &format!("/{}", encode(id)));
        match self.call("GET", &path, None) {
            Ok(reply) => {
                let got = reply
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if got != id {
                    return Err(mismatch(id, got));
                }
                let url = session_url(&reply, id);
                self.remember(&reply, Some(id.to_string()), Some(url.clone()));
                (self.hooks.on_event)(
                    "cadence/cloud_session",
                    json!({"session_id": id, "endpoint": url, "adopted": true}),
                );
                Ok(self.identity(id, url, id.to_string()))
            }
            Err(CallErr::Status(404, _)) => Err(mismatch(id, "absent")),
            Err(err) => {
                let msg = call_text(&err);
                self.note(&msg);
                Err(Error::unknown(format!(
                    "devin cloud adopt held last state (unknown): {msg}"
                )))
            }
        }
    }

    fn identity(&self, id: &str, url: String, generation: String) -> Identity {
        Identity {
            thread_id: id.to_string(),
            session_id: id.to_string(),
            model: None,
            effort: None,
            pid: 0,
            endpoint: Some(url),
            generation: Some(generation),
            attach: None,
        }
    }

    fn remember(&self, body: &Value, id: Option<String>, url: Option<String>) {
        let mut st = self.session.lock().unwrap();
        if let Some(id) = id {
            st.id = Some(id);
        }
        if let Some(url) = url {
            st.endpoint = Some(url);
        }
        if let Some(status) = body.get("status").and_then(Value::as_str) {
            st.status = status.to_string();
        }
        if let Some(detail) = body.get("status_detail").and_then(Value::as_str) {
            st.detail = detail.to_string();
        }
        if let Some(n) = body.get("acus_consumed").and_then(Value::as_f64) {
            st.acus = Some(n);
        }
        if let Some(n) = body.get("max_acu_limit").and_then(Value::as_f64) {
            st.limit = Some(n);
        }
        st.seen = assistant_count(body);
        st.turn_text = transcript_after(body, st.message_mark);
    }

    /// Freeze the session as it stands at post time, so a message or a
    /// terminal state that already exists is not this turn's result.
    fn capture_post_marker(&self) {
        let (org, id) = {
            let st = self.session.lock().unwrap();
            let Some(id) = st.id.clone() else {
                return;
            };
            (st.org.clone(), id)
        };
        let path = self.org_path(&org, &format!("/{}", encode(&id)));
        if let Ok(body) = self.call("GET", &path, None) {
            let (status, detail) = status_of(&body);
            let count = assistant_count(&body);
            let mut st = self.session.lock().unwrap();
            st.status = status.clone();
            st.detail = detail.clone();
            st.seen = count;
            st.message_mark = count;
            st.post_status = status;
            st.post_detail = detail;
            st.post_marked = true;
            st.turn_text.clear();
        } else {
            let mut st = self.session.lock().unwrap();
            st.message_mark = st.seen;
            st.post_status = st.status.to_ascii_lowercase();
            st.post_detail = st.detail.to_ascii_lowercase();
            st.post_marked = true;
            st.turn_text.clear();
        }
    }

    fn post_message(&self, text: &str) -> Result<()> {
        let (org, id) = {
            let st = self.session.lock().unwrap();
            let id = st
                .id
                .clone()
                .ok_or_else(|| Error::rejected("devin cloud has no session to message"))?;
            (st.org.clone(), id)
        };
        self.capture_post_marker();
        let path = self.org_path(&org, &format!("/{}/messages", encode(&id)));
        let body = json!({"message": text});
        match self.call("POST", &path, Some(&body)) {
            Ok(_) => Ok(()),
            Err(CallErr::Status(429, detail)) | Err(CallErr::Transport(detail)) => {
                self.note(&detail);
                Err(Error::unknown(format!(
                    "devin cloud poll held last state ({}): {detail}",
                    self.last_state()
                )))
            }
            Err(CallErr::Status(code, detail)) => {
                self.note(&detail);
                Err(Error::provider(format!(
                    "devin cloud message failed: HTTP {code} {detail}"
                )))
            }
        }
    }

    fn poll_once(&self) -> Result<Phase> {
        let (org, id) = {
            let st = self.session.lock().unwrap();
            let id = st
                .id
                .clone()
                .ok_or_else(|| Error::rejected("devin cloud has no session to poll"))?;
            (st.org.clone(), id)
        };
        let path = self.org_path(&org, &format!("/{}", encode(&id)));
        match self.call("GET", &path, None) {
            Ok(body) => {
                let stale = {
                    let st = self.session.lock().unwrap();
                    let (status, detail) = status_of(&body);
                    st.post_marked
                        && assistant_count(&body) <= st.message_mark
                        && status == st.post_status
                        && detail == st.post_detail
                };
                self.remember(&body, None, None);
                let phase = phase_of(&body, stale);
                if !matches!(phase, Phase::Wait) {
                    self.session.lock().unwrap().wait_sent = false;
                }
                if matches!(phase, Phase::Wait) {
                    self.emit_wait(&body);
                }
                Ok(phase)
            }
            Err(err) => {
                if self.released() {
                    return Ok(Phase::Interrupted);
                }
                let msg = call_text(&err);
                self.note(&msg);
                *self.hold_detail.lock().unwrap() = msg.clone();
                let held = self.last_state();
                match err {
                    CallErr::Status(429, _) | CallErr::Transport(_) => Err(Error::unknown(
                        format!("devin cloud poll held last state ({held}): {msg}"),
                    )),
                    CallErr::Status(code, _) if code >= 500 => Err(Error::unknown(format!(
                        "devin cloud poll held last state ({held}): {msg}"
                    ))),
                    CallErr::Status(code, _) => Err(Error::provider(format!(
                        "devin cloud poll failed: HTTP {code} {msg}"
                    ))),
                }
            }
        }
    }

    fn emit_wait(&self, body: &Value) {
        let id = {
            let mut st = self.session.lock().unwrap();
            if st.wait_sent {
                return;
            }
            st.wait_sent = true;
            st.id.clone().unwrap_or_default()
        };
        (self.hooks.on_request)(ProviderRequest {
            id: json!(id),
            method: "devin/user_input".into(),
            params: json!({
                "session_id": id,
                "text": latest_assistant(body),
            }),
        });
    }

    fn last_state(&self) -> String {
        let st = self.session.lock().unwrap();
        if st.detail.is_empty() {
            if st.status.is_empty() {
                "unknown".into()
            } else {
                st.status.clone()
            }
        } else {
            st.detail.clone()
        }
    }

    fn released(&self) -> bool {
        self.release.load(Ordering::SeqCst)
    }

    fn wait_interval(&self) -> bool {
        let step = Duration::from_millis(20);
        let start = Instant::now();
        while start.elapsed() < self.interval {
            if self.released() {
                return true;
            }
            let left = self.interval.saturating_sub(start.elapsed());
            thread::sleep(step.min(left));
        }
        self.released()
    }

    fn failed_turn(&self, turn_id: &str, msg: String) -> TurnResult {
        TurnResult {
            turn_id: turn_id.to_string(),
            status: "failed".into(),
            text: String::new(),
            stop_reason: Some("error".into()),
            error: Some(msg),
        }
    }

    fn interrupted(&self, turn_id: &str) -> TurnResult {
        TurnResult {
            turn_id: turn_id.to_string(),
            status: "interrupted".into(),
            text: String::new(),
            stop_reason: Some("interrupted".into()),
            error: None,
        }
    }

    fn delete_session(&self) {
        let (org, id) = {
            let st = self.session.lock().unwrap();
            match st.id.clone() {
                Some(id) if !st.terminated => (st.org.clone(), id),
                _ => return,
            }
        };
        let path = self.org_path(&org, &format!("/{}", encode(&id)));
        match self.call("DELETE", &path, None) {
            Ok(_) => {
                self.session.lock().unwrap().terminated = true;
            }
            Err(err) => self.note(&call_text(&err)),
        }
    }

    fn archive_session(&self) {
        let (org, id, terminated) = {
            let st = self.session.lock().unwrap();
            (st.org.clone(), st.id.clone(), st.terminated)
        };
        if terminated {
            return;
        }
        let Some(id) = id else {
            return;
        };
        let path = self.org_path(&org, &format!("/{}/archive", encode(&id)));
        if let Err(err) = self.call("POST", &path, Some(&json!({}))) {
            self.note(&call_text(&err));
        }
    }
}

impl ProviderAdapter for DevinCloudAdapter {
    fn open(&self, agent: &Agent) -> Result<Identity> {
        let (_key, org) = self.creds()?;
        *self.cwd.lock().unwrap() = agent.cwd.clone();
        self.session.lock().unwrap().org = org.clone();
        if let Some(id) = existing_session(agent)? {
            return self.bind_existing(&org, &id);
        }
        let repos = string_param(agent, "repos");
        self.preflight(&org, &repos)?;
        self.create(&org, agent, &repos)
    }

    fn open_adopted(
        &self,
        _agent: &Agent,
        adoption: &crate::store::AdoptEntry,
    ) -> Result<Identity> {
        let (_key, org) = self.creds()?;
        self.session.lock().unwrap().org = org.clone();
        let id = if adoption.native_session.is_empty() {
            adoption.generation.as_str()
        } else {
            adoption.native_session.as_str()
        };
        if !adoption.generation.is_empty()
            && !adoption.native_session.is_empty()
            && adoption.generation != adoption.native_session
        {
            return Err(mismatch(&adoption.generation, &adoption.native_session));
        }
        let ident = self.bind_existing(&org, id)?;
        if !adoption.generation.is_empty()
            && ident.generation.as_deref() != Some(adoption.generation.as_str())
        {
            return Err(mismatch(&adoption.generation, id));
        }
        Ok(ident)
    }

    fn run_turn(
        &self,
        prompt: &str,
        client_message_id: &str,
        on_started: &dyn Fn(&str),
    ) -> Result<TurnResult> {
        if self.released() {
            return Ok(self.interrupted(client_message_id));
        }
        let cwd = self.cwd.lock().unwrap().clone();
        let prompt = scrub_path(&cwd, prompt.to_string());
        match self.post_message(&prompt) {
            Ok(()) => {}
            Err(Error::Provider(msg)) => {
                return Ok(self.failed_turn(client_message_id, msg));
            }
            Err(err) => {
                // Stop flipped the flag while the post was in flight.
                // Ending the turn interrupted lets close archive; it does
                // not retry a message that may already have landed.
                if self.released() {
                    return Ok(self.interrupted(client_message_id));
                }
                return Err(err);
            }
        }
        on_started(client_message_id);
        let deadline = Instant::now() + self.budget;
        let mut backoff = self.interval;
        loop {
            if self.released() {
                return Ok(self.interrupted(client_message_id));
            }
            if Instant::now() >= deadline {
                let detail = self.hold_detail.lock().unwrap().clone();
                let detail = if detail.is_empty() {
                    "poll budget exhausted".to_string()
                } else {
                    detail
                };
                return Err(Error::unknown(format!(
                    "devin cloud poll held last state ({}): {detail}",
                    self.last_state()
                )));
            }
            match self.poll_once() {
                Ok(Phase::Running) | Ok(Phase::Wait) => {
                    backoff = self.interval;
                }
                Ok(Phase::Done) => {
                    return Ok(TurnResult {
                        turn_id: client_message_id.to_string(),
                        status: "completed".into(),
                        text: self.turn_text(),
                        stop_reason: Some("finished".into()),
                        error: None,
                    });
                }
                Ok(Phase::Failed) => {
                    let text = self.turn_text();
                    return Ok(TurnResult {
                        turn_id: client_message_id.to_string(),
                        status: "failed".into(),
                        text,
                        stop_reason: Some("error".into()),
                        error: Some(self.last_state()),
                    });
                }
                Ok(Phase::Interrupted) => return Ok(self.interrupted(client_message_id)),
                Err(Error::Provider(msg)) => return Ok(self.failed_turn(client_message_id, msg)),
                Err(Error::OutcomeUnknown(_)) => {
                    let delay = backoff;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                    if self.pause(delay) {
                        return Ok(self.interrupted(client_message_id));
                    }
                    continue;
                }
                Err(err) => return Err(err),
            }
            if self.wait_interval() {
                return Ok(self.interrupted(client_message_id));
            }
        }
    }

    fn poll_interval(&self) -> Duration {
        self.interval
    }

    fn recover_budget(&self) -> Duration {
        self.recover_budget
    }

    fn poll_settled(&self) -> Result<SettledPoll> {
        if self.released() {
            return Ok(SettledPoll::Ready(self.interrupted("cloud-recover")));
        }
        match self.poll_once() {
            Ok(Phase::Done) => Ok(SettledPoll::Ready(TurnResult {
                turn_id: "cloud-recover".into(),
                status: "completed".into(),
                text: self.turn_text(),
                stop_reason: Some("finished".into()),
                error: None,
            })),
            Ok(Phase::Failed) => Ok(SettledPoll::Ready(TurnResult {
                turn_id: "cloud-recover".into(),
                status: "failed".into(),
                text: self.turn_text(),
                stop_reason: Some("error".into()),
                error: Some(self.last_state()),
            })),
            Ok(Phase::Interrupted) => Ok(SettledPoll::Ready(self.interrupted("cloud-recover"))),
            Ok(Phase::Running) | Ok(Phase::Wait) => Ok(SettledPoll::Pending { transient: false }),
            Err(Error::OutcomeUnknown(_)) => Ok(SettledPoll::Pending { transient: true }),
            Err(Error::Provider(msg)) => {
                Ok(SettledPoll::Ready(self.failed_turn("cloud-recover", msg)))
            }
            Err(err) => Err(err),
        }
    }

    fn respond(&self, _request_id: &Value, result: Value) -> Result<()> {
        let text = result
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| result.get("text").and_then(Value::as_str))
            .or_else(|| result.as_str())
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| Error::rejected("devin cloud respond needs a message string"))?;
        self.post_message(text)
    }

    fn interrupt(&self) {
        self.release.store(true, Ordering::SeqCst);
        self.delete_session();
    }

    fn release_for_stop(&self) {
        self.release.store(true, Ordering::SeqCst);
    }

    fn disconnected(&self) -> bool {
        false
    }

    fn close(&self) {
        self.release.store(true, Ordering::SeqCst);
        self.archive_session();
    }

    fn detach(&self) {
        // Daemon shutdown must not archive or delete: the session id
        // stays on the agent row and the next open adopts it.
        self.release.store(true, Ordering::SeqCst);
    }

    fn quota_snapshot(&self) -> Option<Value> {
        let st = self.session.lock().unwrap();
        st.id.as_ref()?;
        let mut data = Map::new();
        if !st.org.is_empty() {
            data.insert("accountId".into(), json!(st.org));
        }
        if let Some(n) = st.acus {
            data.insert("acus_consumed".into(), json!(n));
        }
        if let Some(n) = st.limit {
            data.insert("max_acu_limit".into(), json!(n));
        }
        Some(json!({
            "source": "devin/session",
            "state": "available",
            "data": Value::Object(data),
        }))
    }
}

fn configured(env: &ProviderEnv, cadence_name: &str, plain: &str) -> Option<String> {
    if let Some(owned) = env.own(cadence_name) {
        return nonempty(owned);
    }
    if let Ok(value) = std::env::var(cadence_name) {
        // A blank override is intentional and must not reveal DEVIN_*.
        return nonempty(value);
    }
    if cadence_name != plain {
        if let Some(owned) = env.own(plain) {
            return nonempty(owned);
        }
        if let Ok(value) = std::env::var(plain) {
            return nonempty(value);
        }
    }
    None
}

fn nonempty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn api_base(env: &ProviderEnv) -> String {
    env.var("CADENCE_DEVIN_API_BASE")
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
}

fn millis(env: &ProviderEnv, name: &str, default: u64) -> u64 {
    env.var(name)
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

fn scrub(key: &str, text: String) -> String {
    if key.is_empty() {
        text
    } else {
        text.replace(key, "[redacted]")
    }
}

fn bounded(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    const LIMIT: usize = 180;
    if flat.chars().count() <= LIMIT {
        return flat;
    }
    let mut out: String = flat.chars().take(LIMIT).collect();
    out.push('…');
    out
}

fn call_text(err: &CallErr) -> String {
    match err {
        CallErr::Status(code, detail) => format!("HTTP {code} {detail}"),
        CallErr::Transport(detail) => detail.clone(),
    }
}

fn encode(segment: &str) -> String {
    let mut out = String::new();
    for b in segment.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn split_repo(repo: &str) -> Result<(&str, &str)> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(Error::rejected(format!(
            "repos entries must be owner/name, got '{repo}'"
        )));
    };
    if owner.is_empty()
        || name.is_empty()
        || name.contains('/')
        || owner.contains(char::is_whitespace)
        || name.contains(char::is_whitespace)
    {
        return Err(Error::rejected(format!(
            "repos entries must be owner/name, got '{repo}'"
        )));
    }
    Ok((owner, name))
}

fn repo_enabled(body: &Value, owner: &str, name: &str) -> bool {
    let want = format!("{owner}/{name}");
    let items = body
        .get("repositories")
        .or_else(|| body.get("data"))
        .or_else(|| body.get("items"))
        .and_then(Value::as_array)
        .or_else(|| body.as_array());
    let Some(items) = items else {
        return false;
    };
    items.iter().any(|item| {
        if item.as_str() == Some(want.as_str()) || item.as_str() == Some(name) {
            return true;
        }
        let full = item.get("full_name").and_then(Value::as_str).unwrap_or("");
        let item_name = item.get("name").and_then(Value::as_str).unwrap_or("");
        let item_owner = item
            .get("owner")
            .and_then(Value::as_str)
            .or_else(|| item.get("org").and_then(Value::as_str))
            .unwrap_or("");
        full == want || (item_name == name && (item_owner.is_empty() || item_owner == owner))
    })
}

fn require_session_id(id: &str) -> Result<()> {
    if id.starts_with("devin-") && id.len() > "devin-".len() {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "devin cloud session id must look like 'devin-…', got '{id}'"
        )))
    }
}

fn mismatch(want: &str, got: &str) -> Error {
    Error::rejected(format!(
        "GenerationMismatch: recorded session '{want}' does not match provider session '{got}' — refusing to adopt"
    ))
}

fn existing_session(agent: &Agent) -> Result<Option<String>> {
    let from_param = agent
        .params
        .as_ref()
        .and_then(|p| p.get("session"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let from_row = agent.session_id.as_ref().filter(|s| !s.is_empty()).cloned();
    match (from_param, from_row) {
        (Some(param), Some(row)) if param != row => Err(mismatch(&row, &param)),
        (Some(param), _) => {
            require_session_id(&param)?;
            Ok(Some(param))
        }
        (None, Some(row)) => {
            require_session_id(&row)?;
            Ok(Some(row))
        }
        (None, None) => Ok(None),
    }
}

fn str_param(agent: &Agent, key: &str) -> Option<String> {
    agent
        .params
        .as_ref()
        .and_then(|p| p.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn string_param(agent: &Agent, key: &str) -> Vec<String> {
    let Some(value) = agent.params.as_ref().and_then(|p| p.get(key)) else {
        return Vec::new();
    };
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        Value::String(s) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn bool_param(agent: &Agent, key: &str) -> bool {
    match agent.params.as_ref().and_then(|p| p.get(key)) {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => matches!(value.as_str(), "true" | "1" | "yes"),
        _ => false,
    }
}

fn acu_limit(agent: &Agent) -> f64 {
    let Some(value) = agent.params.as_ref().and_then(|p| p.get("max_acu_limit")) else {
        return DEFAULT_MAX_ACU_LIMIT as f64;
    };
    let parsed = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()));
    parsed
        .filter(|n| n.is_finite() && *n > 0.0)
        .unwrap_or(DEFAULT_MAX_ACU_LIMIT as f64)
}

fn acu_json(n: f64) -> Value {
    if n.fract() == 0.0 && (0.0..u64::MAX as f64).contains(&n) {
        json!(n as u64)
    } else {
        json!(n)
    }
}

fn open_prompt(agent: &Agent) -> String {
    let mut text = agent.instructions.clone().unwrap_or_default();
    if text.trim().is_empty() {
        text = "You are a Cadence-managed Devin cloud session. The task arrives as the next message. Work in the repository attached to this session.".into();
    }
    scrub_path(&agent.cwd, text)
}

fn scrub_path(cwd: &str, text: String) -> String {
    if cwd.is_empty() {
        text
    } else {
        text.replace(cwd, "(session repo)")
    }
}

fn session_url(body: &Value, id: &str) -> String {
    body.get("url")
        .or_else(|| body.get("session_url"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://app.devin.ai/sessions/{id}"))
}

fn status_of(body: &Value) -> (String, String) {
    let status = body
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let detail = body
        .get("status_detail")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    (status, detail)
}

fn success_terminal(status: &str, detail: &str) -> bool {
    detail == "finished" || status == "finished" || status == "completed"
}

fn failure_terminal(status: &str, detail: &str) -> bool {
    matches!(
        detail,
        "error" | "failed" | "expired" | "blocked" | "acu_exhausted" | "out_of_acu"
    ) || detail.contains("acu")
        || matches!(status, "error" | "failed" | "expired" | "blocked")
}

fn waiting_detail(detail: &str) -> bool {
    detail == "waiting_for_user" || detail == "waiting_for_approval"
}

/// `stale` means this status, detail, and assistant count were already
/// present when we posted. A new error, expiry, block, or ACU exhaustion
/// settles even when the session wrote no new assistant message.
fn phase_of(body: &Value, stale: bool) -> Phase {
    let (status, detail) = status_of(body);
    let terminal = success_terminal(&status, &detail)
        || failure_terminal(&status, &detail)
        || detail == "interrupted"
        || status == "interrupted";
    if stale && (terminal || waiting_detail(&detail)) {
        return Phase::Running;
    }
    if success_terminal(&status, &detail) {
        Phase::Done
    } else if failure_terminal(&status, &detail) {
        Phase::Failed
    } else if waiting_detail(&detail) {
        Phase::Wait
    } else if detail == "interrupted" || status == "interrupted" {
        Phase::Interrupted
    } else {
        Phase::Running
    }
}

fn create_claim(alias: &str) -> String {
    static NONCE: AtomicU64 = AtomicU64::new(1);
    let n = NONCE.fetch_add(1, Ordering::Relaxed);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("cadence-agent:{alias}:{tick:x}-{n:x}")
}

fn latest_assistant(body: &Value) -> String {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return String::new();
    };
    messages
        .iter()
        .rev()
        .filter(|m| {
            matches!(
                m.get("role").and_then(Value::as_str),
                Some("assistant") | Some("devin") | None
            )
        })
        .find_map(|m| {
            m.get("message")
                .or_else(|| m.get("text"))
                .or_else(|| m.get("content"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn is_assistant(message: &Value) -> bool {
    matches!(
        message.get("role").and_then(Value::as_str),
        Some("assistant") | Some("devin") | None
    )
}

fn assistant_count(body: &Value) -> usize {
    body.get("messages")
        .and_then(Value::as_array)
        .map(|messages| messages.iter().filter(|m| is_assistant(m)).count())
        .unwrap_or(0)
}

fn transcript_after(body: &Value, mark: usize) -> String {
    let mut lines = Vec::new();
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages.iter().filter(|m| is_assistant(m)).skip(mark) {
            if let Some(text) = message
                .get("message")
                .or_else(|| message.get("text"))
                .or_else(|| message.get("content"))
                .and_then(Value::as_str)
            {
                if !text.is_empty() {
                    lines.push(text.to_string());
                }
            }
        }
    }
    if let Some(prs) = body.get("pull_requests").and_then(Value::as_array) {
        for pr in prs {
            if let Some(url) = pr
                .get("url")
                .and_then(Value::as_str)
                .or_else(|| pr.as_str())
            {
                lines.push(url.to_string());
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use serde_json::{json, Value};

    use super::*;
    use crate::adapter::{build, AdapterHooks, ProviderEnv, ProviderRequest};
    use crate::store::{AdoptEntry, Agent};

    const KEY: &str = "cog_test_secret_value";
    const ORG: &str = "org-test";
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const CWD: &str = "/secret/host/path";

    fn must_err<T>(result: std::result::Result<T, crate::error::Error>) -> crate::error::Error {
        match result {
            Err(err) => err,
            Ok(_) => panic!("expected an error"),
        }
    }

    trait PipeErr<T> {
        fn pipe_err(self) -> crate::error::Error;
    }

    impl<T> PipeErr<T> for std::result::Result<T, crate::error::Error> {
        fn pipe_err(self) -> crate::error::Error {
            must_err(self)
        }
    }

    #[derive(Clone, Copy)]
    enum Script {
        Happy,
        MissingRepo,
        Wait,
        ErrorDetail,
        Interrupted,
        RateLimit,
        Poll500,
        Create500,
        AdoptOk,
        AdoptMismatch,
        Adopt404,
        Preflight429,
        Create429,
        Create429Forever,
        StaleSha,
        ErrorQuiet,
        StaleWait,
    }

    struct Hit {
        method: String,
        path: String,
        body: String,
        auth: String,
    }

    struct Mock {
        hits: Arc<Mutex<Vec<Hit>>>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn session_body(id: &str, detail: &str, text: &str) -> String {
        json!({
            "session_id": id,
            "status": "running",
            "status_detail": detail,
            "url": format!("https://app.devin.ai/sessions/{id}"),
            "messages": [{"role": "assistant", "message": text}],
            "pull_requests": [{"url": "https://github.com/favcrm/cadence/pull/9"}],
            "acus_consumed": 1.25,
            "max_acu_limit": 10,
        })
        .to_string()
    }

    fn created_body(id: &str) -> String {
        json!({
            "session_id": id,
            "status": "running",
            "status_detail": "working",
            "url": format!("https://app.devin.ai/sessions/{id}"),
        })
        .to_string()
    }

    fn session_messages(id: &str, detail: &str, texts: &[&str]) -> String {
        let messages: Vec<Value> = texts
            .iter()
            .map(|text| json!({"role": "assistant", "message": text}))
            .collect();
        json!({
            "session_id": id,
            "status": "running",
            "status_detail": detail,
            "url": format!("https://app.devin.ai/sessions/{id}"),
            "messages": messages,
            "acus_consumed": 1.25,
            "max_acu_limit": 10,
        })
        .to_string()
    }

    fn message_posts(hits: &[Hit]) -> usize {
        hits.iter()
            .filter(|hit| hit.method == "POST" && hit.path.contains("/messages"))
            .count()
    }

    fn reply(script: Script, method: &str, path: &str, hits: &[Hit]) -> Option<(u16, String)> {
        if path.contains("/repositories") {
            if matches!(script, Script::Preflight429) {
                let checks = hits
                    .iter()
                    .filter(|hit| hit.path.contains("/repositories"))
                    .count();
                if checks <= 1 {
                    return Some((429, json!({"error": KEY}).to_string()));
                }
            }
            let missing =
                matches!(script, Script::MissingRepo) && path.contains("filter_name=missing");
            let body = if missing {
                json!({"repositories": []}).to_string()
            } else {
                json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]}).to_string()
            };
            return Some((200, body));
        }
        if method == "POST" && path.ends_with("/sessions") {
            if matches!(script, Script::Create500) {
                return Some((500, json!({"error": KEY}).to_string()));
            }
            if matches!(script, Script::Create429 | Script::Create429Forever) {
                let posts = hits
                    .iter()
                    .filter(|hit| hit.method == "POST" && hit.path.ends_with("/sessions"))
                    .count();
                if matches!(script, Script::Create429Forever) || posts <= 2 {
                    return Some((429, json!({"error": KEY}).to_string()));
                }
            }
            return Some((200, created_body("devin-created")));
        }
        if method == "POST" && path.contains("/messages") {
            return Some((200, json!({"ok": true}).to_string()));
        }
        if method == "DELETE" {
            return Some((200, json!({"ok": true}).to_string()));
        }
        if method == "POST" && path.ends_with("/archive") {
            return Some((200, json!({"ok": true}).to_string()));
        }
        if method == "GET" && path.contains("/sessions/") {
            return Some(match script {
                Script::RateLimit => (429, json!({"error": KEY}).to_string()),
                Script::Poll500 => (500, json!({"error": KEY}).to_string()),
                Script::Adopt404 => (404, json!({"error": "not found"}).to_string()),
                Script::AdoptMismatch => (200, session_body("devin-other", "working", "")),
                Script::AdoptOk => (200, session_body("devin-keep", "working", "")),
                Script::Wait => {
                    let posts = message_posts(hits);
                    if posts == 0 {
                        (200, created_body("devin-created"))
                    } else if posts == 1 {
                        (
                            200,
                            session_body("devin-created", "waiting_for_user", "which approach?"),
                        )
                    } else {
                        (
                            200,
                            session_messages(
                                "devin-created",
                                "finished",
                                &["which approach?", &format!("SHA: {SHA}")],
                            ),
                        )
                    }
                }
                Script::ErrorDetail => {
                    if message_posts(hits) == 0 {
                        (200, created_body("devin-created"))
                    } else {
                        (200, session_body("devin-created", "error", "boom"))
                    }
                }
                Script::ErrorQuiet => {
                    if message_posts(hits) == 0 {
                        (200, created_body("devin-created"))
                    } else {
                        (
                            200,
                            json!({
                                "session_id": "devin-created",
                                "status": "error",
                                "status_detail": "error",
                                "url": "https://app.devin.ai/sessions/devin-created",
                                "messages": [],
                            })
                            .to_string(),
                        )
                    }
                }
                Script::StaleWait => (
                    200,
                    session_body("devin-created", "waiting_for_user", "previous question"),
                ),
                Script::Interrupted => {
                    if message_posts(hits) == 0 {
                        (200, created_body("devin-created"))
                    } else {
                        (200, session_body("devin-created", "interrupted", ""))
                    }
                }
                Script::StaleSha => {
                    let posts = message_posts(hits);
                    if posts == 0 {
                        (200, created_body("devin-created"))
                    } else if posts == 1 {
                        (
                            200,
                            session_body("devin-created", "finished", &format!("SHA: {SHA}")),
                        )
                    } else {
                        (
                            200,
                            session_messages(
                                "devin-created",
                                "finished",
                                &[&format!("SHA: {SHA}"), "revision two has no trailer"],
                            ),
                        )
                    }
                }
                _ => {
                    if message_posts(hits) == 0 {
                        (200, created_body("devin-created"))
                    } else {
                        (
                            200,
                            session_body("devin-created", "finished", &format!("SHA: {SHA}")),
                        )
                    }
                }
            });
        }
        Some((500, json!({"error": "unexpected"}).to_string()))
    }

    fn start_mock(script: Script) -> (String, Mock) {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind mock");
        let addr = server.server_addr().to_ip().expect("ip listen");
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_t = Arc::clone(&hits);
        let stop_t = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                let mut req = match server.recv_timeout(Duration::from_millis(50)) {
                    Ok(Some(req)) => req,
                    _ => continue,
                };
                let path = req.url().to_string();
                let method = req.method().as_str().to_string();
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let auth = req
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Authorization"))
                    .map(|header| header.value.as_str().trim().to_string())
                    .unwrap_or_default();
                let response = {
                    let mut guard = hits_t.lock().unwrap();
                    guard.push(Hit {
                        method: method.clone(),
                        path: path.clone(),
                        body,
                        auth,
                    });
                    reply(script, &method, &path, &guard)
                };
                if let Some((status, payload)) = response {
                    let _ = req.respond(
                        tiny_http::Response::from_string(payload).with_status_code(status),
                    );
                }
            }
        });
        (
            format!("http://{addr}"),
            Mock {
                hits,
                stop,
                thread: Some(thread),
            },
        )
    }

    fn env_at(base: &str) -> ProviderEnv {
        let env = ProviderEnv::default();
        env.set("CADENCE_DEVIN_API_KEY", KEY);
        env.set("CADENCE_DEVIN_ORG_ID", ORG);
        env.set("CADENCE_DAEMON_ID", "daemon-1");
        env.set("CADENCE_DEVIN_API_BASE", base);
        env.set("CADENCE_DEVIN_POLL_INTERVAL_MS", "40");
        env.set("CADENCE_DEVIN_POLL_BUDGET_MS", "400");
        env
    }

    fn hooks(
        events: &Arc<Mutex<Vec<Value>>>,
        requests: &Arc<Mutex<Vec<ProviderRequest>>>,
    ) -> AdapterHooks {
        let events = Arc::clone(events);
        let requests = Arc::clone(requests);
        AdapterHooks {
            on_event: Box::new(move |_method, params| events.lock().unwrap().push(params)),
            on_request: Box::new(move |request| requests.lock().unwrap().push(request)),
        }
    }

    fn agent(params: Value) -> Agent {
        Agent {
            alias: "cloud-1".into(),
            provider: "devin".into(),
            endpoint_kind: "cloud".into(),
            role: "worker".into(),
            team_role: None,
            cwd: CWD.into(),
            sandbox: "read-only".into(),
            instructions: Some(format!("work in {CWD}")),
            thread_id: None,
            session_id: None,
            model: None,
            effort: None,
            pid: None,
            endpoint: None,
            params: Some(params),
            model_selection: None,
            quota: None,
            generation: None,
            state: "starting".into(),
            enabled: true,
            error: None,
            created: 0.0,
            updated: 0.0,
        }
    }

    struct Harness {
        adapter: DevinCloudAdapter,
        mock: Mock,
        events: Arc<Mutex<Vec<Value>>>,
        requests: Arc<Mutex<Vec<ProviderRequest>>>,
    }

    fn adapter_for(script: Script) -> Harness {
        let (base, mock) = start_mock(script);
        let events = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let adapter = DevinCloudAdapter::new(hooks(&events, &requests), &env_at(&base));
        Harness {
            adapter,
            mock,
            events,
            requests,
        }
    }

    fn assert_hygiene(adapter: &DevinCloudAdapter, hits: &[Hit], events: &[Value], params: &Value) {
        let log = adapter.log.lock().unwrap().clone();
        assert!(!log.contains(KEY), "provider log contained the api key");
        assert!(
            log.len() <= LOG_LIMIT + 8,
            "provider log grew without a bound"
        );
        assert!(
            !events.iter().any(|event| event.to_string().contains(KEY)),
            "event contained the api key"
        );
        assert!(
            !params.to_string().contains(KEY),
            "launch params contained the api key"
        );
        assert!(!hits.is_empty(), "expected at least one http call");
        for hit in hits {
            assert!(
                !hit.path.contains(KEY),
                "request path contained the api key"
            );
            assert!(
                !hit.body.contains(KEY),
                "request body contained the api key"
            );
            assert_eq!(hit.auth, format!("Bearer {KEY}"));
        }
    }

    fn hits_of(mock: &Mock) -> Vec<Hit> {
        mock.hits.lock().unwrap().drain(..).collect()
    }

    #[test]
    fn create_binds_identity_default_limit_and_scrubs_the_host_path() {
        let Harness {
            adapter,
            mock,
            events,
            ..
        } = adapter_for(Script::Happy);
        let worker = agent(json!({
            "repos": ["favcrm/cadence"],
            "devin_mode": "fast",
            "tags": ["team"],
        }));
        let ident = adapter.open(&worker).unwrap();
        assert_eq!(ident.session_id, "devin-created");
        assert_eq!(ident.thread_id, "devin-created");
        assert_eq!(ident.generation.as_deref(), Some("devin-created"));
        assert_eq!(ident.pid, 0);
        assert_eq!(
            ident.endpoint.as_deref(),
            Some("https://app.devin.ai/sessions/devin-created")
        );
        let turn = adapter
            .run_turn(&format!("continue {CWD}"), "turn-1", &|_| {})
            .unwrap();
        assert_eq!(turn.status, "completed");
        assert!(turn.text.contains(&format!("SHA: {SHA}")));
        assert!(turn
            .text
            .contains("https://github.com/favcrm/cadence/pull/9"));
        let hits = hits_of(&mock);
        assert_hygiene(
            &adapter,
            &hits,
            &events.lock().unwrap(),
            worker.params.as_ref().unwrap(),
        );
        let create = hits
            .iter()
            .find(|hit| hit.method == "POST" && hit.path.ends_with("/sessions"))
            .expect("create");
        let body: Value = serde_json::from_str(&create.body).unwrap();
        assert_eq!(body["max_acu_limit"], json!(DEFAULT_MAX_ACU_LIMIT));
        assert_eq!(body["devin_mode"], "fast");
        assert_eq!(body["repos"], json!(["favcrm/cadence"]));
        assert!(body["prompt"].as_str().unwrap().contains("(session repo)"));
        assert!(!body["prompt"].as_str().unwrap().contains(CWD));
        let tags = body["tags"].as_array().unwrap();
        assert!(tags.iter().any(|tag| tag == "cadence:daemon-1"));
        assert!(tags.iter().any(|tag| tag == "team"));
        let message = hits
            .iter()
            .find(|hit| hit.method == "POST" && hit.path.contains("/messages"))
            .expect("message");
        assert!(message.body.contains("(session repo)"));
        assert!(!message.body.contains(CWD));
        let quota = adapter.quota_snapshot().unwrap();
        assert_eq!(quota["source"], "devin/session");
        assert_eq!(quota["data"]["accountId"], ORG);
        assert_eq!(quota["data"]["acus_consumed"], json!(1.25));
        assert!(!quota.to_string().contains(KEY));
        assert!(hits.iter().any(|hit| hit.path.contains("/repositories")));
        assert!(hits.iter().all(|hit| hit.method != "DELETE"));
    }

    #[test]
    fn explicit_acu_limit_overrides_the_default() {
        let Harness { adapter, mock, .. } = adapter_for(Script::Happy);
        let worker = agent(json!({"repos": ["favcrm/cadence"], "max_acu_limit": 4}));
        adapter.open(&worker).unwrap();
        let hits = hits_of(&mock);
        let create = hits
            .iter()
            .find(|hit| hit.method == "POST" && hit.path.ends_with("/sessions"))
            .unwrap();
        let body: Value = serde_json::from_str(&create.body).unwrap();
        assert_eq!(body["max_acu_limit"], json!(4));
    }

    #[test]
    fn missing_repo_is_refused_before_create() {
        let Harness { adapter, mock, .. } = adapter_for(Script::MissingRepo);
        let worker = agent(json!({"repos": ["favcrm/cadence", "favcrm/missing"]}));
        let err = adapter.open(&worker).pipe_err();
        assert_eq!(err.kind(), "rejected");
        let text = err.to_string();
        assert!(
            text.contains("not enabled in Devin git connection"),
            "{text}"
        );
        assert!(text.contains("favcrm/missing"), "{text}");
        let hits = hits_of(&mock);
        assert!(hits
            .iter()
            .any(|hit| hit.path.contains("filter_name=missing")));
        assert!(hits
            .iter()
            .all(|hit| !(hit.method == "POST" && hit.path.ends_with("/sessions"))));
    }

    #[test]
    fn waiting_for_user_emits_one_request_and_respond_posts_the_answer() {
        let Harness {
            adapter,
            mock,
            requests,
            ..
        } = adapter_for(Script::Wait);
        let worker = agent(json!({"repos": ["favcrm/cadence"]}));
        adapter.open(&worker).unwrap();
        let turn = thread::scope(|scope| {
            let handle = scope.spawn(|| adapter.run_turn("do the task", "turn-1", &|_| {}));
            let start = std::time::Instant::now();
            while requests.lock().unwrap().is_empty() {
                assert!(
                    start.elapsed() < Duration::from_secs(3),
                    "no user-input request"
                );
                thread::sleep(Duration::from_millis(10));
            }
            let request = requests.lock().unwrap()[0].clone();
            assert_eq!(request.method, "devin/user_input");
            assert_eq!(request.params["text"], "which approach?");
            adapter
                .respond(&request.id, json!({"message": "ship it"}))
                .unwrap();
            handle.join().unwrap()
        })
        .unwrap();
        assert_eq!(turn.status, "completed");
        assert!(turn.text.contains(&format!("SHA: {SHA}")));
        assert_eq!(requests.lock().unwrap().len(), 1);
        let hits = hits_of(&mock);
        assert!(hits.iter().any(|hit| hit.body.contains("ship it")));
        assert_hygiene(&adapter, &hits, &[], worker.params.as_ref().unwrap());
    }

    #[test]
    fn error_without_a_new_message_settles_failed() {
        let Harness {
            adapter,
            mock,
            requests,
            ..
        } = adapter_for(Script::ErrorQuiet);
        adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        let started = Instant::now();
        let turn = adapter.run_turn("do the task", "turn-1", &|_| {}).unwrap();
        assert_eq!(turn.status, "failed");
        assert!(
            started.elapsed() < Duration::from_millis(350),
            "error with no new message kept polling for {:?}",
            started.elapsed()
        );
        assert!(requests.lock().unwrap().is_empty());
        assert!(hits_of(&mock).iter().all(|hit| hit.method != "DELETE"));
        assert!(!adapter.disconnected());
    }

    #[test]
    fn stale_terminal_state_does_not_settle_but_a_new_one_does() {
        for (status, detail) in [
            ("error", "error"),
            ("expired", "expired"),
            ("running", "blocked"),
            ("running", "acu_exhausted"),
        ] {
            let body = json!({
                "status": status,
                "status_detail": detail,
                "messages": [],
            });
            assert!(
                matches!(phase_of(&body, false), Phase::Failed),
                "{status}/{detail} should settle"
            );
            assert!(
                matches!(phase_of(&body, true), Phase::Running),
                "{status}/{detail} leftover should not settle"
            );
        }
        let waiting = json!({
            "status": "running",
            "status_detail": "waiting_for_user",
            "messages": [{"role": "assistant", "message": "previous question"}],
        });
        assert!(matches!(phase_of(&waiting, true), Phase::Running));
        assert!(matches!(phase_of(&waiting, false), Phase::Wait));
    }

    #[test]
    fn leftover_waiting_for_user_does_not_raise_the_previous_message() {
        let Harness {
            adapter,
            requests,
            mock: _mock,
            ..
        } = adapter_for(Script::StaleWait);
        adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        let err = adapter
            .run_turn("do the task", "turn-1", &|_| {})
            .pipe_err();
        assert_eq!(err.kind(), "unknown", "{err}");
        let raised = requests.lock().unwrap().clone();
        assert!(
            raised.is_empty(),
            "stale waiting_for_user raised {:?}",
            raised
                .iter()
                .map(|request| request.params.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn error_detail_fails_the_turn_without_killing_the_session() {
        let Harness { adapter, mock, .. } = adapter_for(Script::ErrorDetail);
        adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        let turn = adapter.run_turn("do the task", "turn-1", &|_| {}).unwrap();
        assert_eq!(turn.status, "failed");
        assert!(turn.text.contains("boom"));
        assert!(hits_of(&mock).iter().all(|hit| hit.method != "DELETE"));
        assert!(!adapter.disconnected());
    }

    #[test]
    fn interrupted_detail_returns_an_interrupted_turn() {
        let Harness { adapter, mock, .. } = adapter_for(Script::Interrupted);
        adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        let turn = adapter.run_turn("do the task", "turn-1", &|_| {}).unwrap();
        assert_eq!(turn.status, "interrupted");
        assert!(hits_of(&mock).iter().all(|hit| hit.method != "DELETE"));
    }

    #[test]
    fn rate_limit_during_poll_holds_state_and_does_not_terminate() {
        let Harness { adapter, mock, .. } = adapter_for(Script::RateLimit);
        adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        let err = adapter
            .run_turn("do the task", "turn-1", &|_| {})
            .pipe_err();
        assert_eq!(err.kind(), "unknown");
        let text = err.to_string();
        assert!(text.contains("held last state (working)"), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
        assert!(!text.contains(KEY), "poll error leaked the api key");
        assert!(!adapter.disconnected());
        let hits = hits_of(&mock);
        assert!(hits.iter().all(|hit| hit.method != "DELETE"));
        assert!(!adapter.log.lock().unwrap().contains(KEY));
    }

    #[test]
    fn poll_server_error_is_held_and_scrubbed() {
        let Harness { adapter, mock, .. } = adapter_for(Script::Poll500);
        adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        let err = adapter
            .run_turn("do the task", "turn-1", &|_| {})
            .pipe_err();
        assert_eq!(err.kind(), "unknown");
        assert!(!err.to_string().contains(KEY));
        assert!(err.to_string().contains("[redacted]"));
        assert!(hits_of(&mock).iter().all(|hit| hit.method != "DELETE"));
        assert!(!adapter.disconnected());
    }

    #[test]
    fn create_server_error_is_scrubbed_and_does_not_bind_a_session() {
        let Harness {
            adapter,
            mock,
            events,
            ..
        } = adapter_for(Script::Create500);
        let worker = agent(json!({"repos": ["favcrm/cadence"]}));
        let err = adapter.open(&worker).pipe_err();
        assert_eq!(err.kind(), "provider");
        assert!(
            !err.to_string().contains(KEY),
            "create error leaked the api key"
        );
        assert!(err.to_string().contains("[redacted]"));
        assert!(adapter.quota_snapshot().is_none());
        let hits = hits_of(&mock);
        assert!(hits.iter().all(|hit| hit.method != "DELETE"));
        assert!(hits
            .iter()
            .all(|hit| !hit.path.contains("/sessions/devin-")));
        assert_hygiene(
            &adapter,
            &hits,
            &events.lock().unwrap(),
            worker.params.as_ref().unwrap(),
        );
    }

    #[test]
    fn missing_credentials_refuse_before_http() {
        let env = ProviderEnv::default();
        env.set("CADENCE_DEVIN_API_KEY", "");
        env.set("CADENCE_DEVIN_ORG_ID", " ");
        env.set("CADENCE_DEVIN_API_BASE", "http://127.0.0.1:9");
        let adapter = DevinCloudAdapter::new(
            hooks(
                &Arc::new(Mutex::new(Vec::new())),
                &Arc::new(Mutex::new(Vec::new())),
            ),
            &env,
        );
        let err = adapter.open(&agent(json!({}))).pipe_err();
        assert_eq!(err.kind(), "rejected");
        let text = err.to_string();
        assert!(text.contains("DEVIN_API_KEY"), "{text}");
        assert!(text.contains("DEVIN_ORG_ID"), "{text}");
        assert!(!text.contains(KEY));
    }

    #[test]
    fn adopt_gets_the_recorded_session_and_refuses_a_mismatch() {
        let Harness { adapter, mock, .. } = adapter_for(Script::AdoptMismatch);
        let err = adapter
            .open(&agent(json!({"session": "devin-want"})))
            .pipe_err();
        assert!(err.to_string().contains("GenerationMismatch"), "{}", err);
        assert!(err.to_string().contains("devin-want"));
        assert!(err.to_string().contains("devin-other"));
        let hits = hits_of(&mock);
        assert!(hits
            .iter()
            .any(|hit| hit.method == "GET" && hit.path.contains("devin-want")));
        assert!(hits.iter().all(|hit| hit.method != "POST"));
    }

    #[test]
    fn adopt_404_is_a_mismatch_not_a_new_session() {
        let Harness { adapter, mock, .. } = adapter_for(Script::Adopt404);
        let err = adapter
            .open(&agent(json!({"session": "devin-gone"})))
            .pipe_err();
        assert!(err.to_string().contains("GenerationMismatch"));
        assert!(err.to_string().contains("absent"));
        assert!(hits_of(&mock).iter().all(|hit| hit.method != "POST"));
    }

    #[test]
    fn adopt_match_reuses_the_session_without_creating() {
        let Harness { adapter, mock, .. } = adapter_for(Script::AdoptOk);
        let ident = adapter
            .open(&agent(json!({"session": "devin-keep"})))
            .unwrap();
        assert_eq!(ident.session_id, "devin-keep");
        assert_eq!(ident.generation.as_deref(), Some("devin-keep"));
        assert_eq!(
            ident.endpoint.as_deref(),
            Some("https://app.devin.ai/sessions/devin-keep")
        );
        let hits = hits_of(&mock);
        assert!(hits
            .iter()
            .any(|hit| hit.method == "GET" && hit.path.contains("devin-keep")));
        assert!(hits.iter().all(|hit| hit.method != "POST"));
    }

    #[test]
    fn open_adopted_refuses_a_generation_mismatch_without_http() {
        let Harness { adapter, mock, .. } = adapter_for(Script::AdoptOk);
        let err = adapter
            .open_adopted(
                &agent(json!({})),
                &AdoptEntry {
                    alias: "cloud-1".into(),
                    message_id: "m1".into(),
                    turn_id: "t1".into(),
                    generation: "devin-a".into(),
                    pane_pid: 0,
                    native_session: "devin-b".into(),
                },
            )
            .pipe_err();
        assert!(err.to_string().contains("GenerationMismatch"));
        assert!(hits_of(&mock).is_empty(), "mismatch must not call the api");
    }

    #[test]
    fn open_adopted_binds_the_recorded_session() {
        let Harness { adapter, mock, .. } = adapter_for(Script::AdoptOk);
        let ident = adapter
            .open_adopted(
                &agent(json!({})),
                &AdoptEntry {
                    alias: "cloud-1".into(),
                    message_id: "m1".into(),
                    turn_id: "t1".into(),
                    generation: "devin-keep".into(),
                    pane_pid: 0,
                    native_session: "devin-keep".into(),
                },
            )
            .unwrap();
        assert_eq!(ident.generation.as_deref(), Some("devin-keep"));
        assert!(hits_of(&mock).iter().all(|hit| hit.method != "POST"));
    }

    #[test]
    fn interrupt_deletes_close_archives_and_detach_is_silent() {
        let Harness { adapter, mock, .. } = adapter_for(Script::Happy);
        adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        let before = mock.hits.lock().unwrap().len();
        adapter.detach();
        assert_eq!(mock.hits.lock().unwrap().len(), before);
        adapter.release.store(false, Ordering::SeqCst);
        adapter.close();
        assert!(mock
            .hits
            .lock()
            .unwrap()
            .iter()
            .any(|hit| hit.path.ends_with("/archive")));
        assert!(mock
            .hits
            .lock()
            .unwrap()
            .iter()
            .all(|hit| hit.method != "DELETE"));
    }

    #[test]
    fn interrupt_terminates_the_remote_session() {
        let Harness { adapter, mock, .. } = adapter_for(Script::Happy);
        adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        adapter.interrupt();
        assert!(mock
            .hits
            .lock()
            .unwrap()
            .iter()
            .any(|hit| hit.method == "DELETE"));
        let before = mock.hits.lock().unwrap().len();
        adapter.close();
        assert_eq!(
            mock.hits.lock().unwrap().len(),
            before,
            "archive must not follow a terminate"
        );
    }

    #[test]
    fn factory_builds_devin_cloud_and_rejects_other_providers() {
        let (base, mock) = start_mock(Script::Happy);
        let events = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let env = env_at(&base);
        let mut worker = agent(json!({"repos": ["favcrm/cadence"]}));
        let log = std::env::temp_dir().join(format!("cad243-cloud-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let built = build(&worker, hooks(&events, &requests), &log, &env).unwrap();
        let ident = built.open(&worker).unwrap();
        assert_eq!(ident.session_id, "devin-created");
        assert!(
            !log.exists(),
            "cloud adapter must not write the provider log file"
        );
        worker.provider = "claude".into();
        let err = build(
            &worker,
            hooks(&events, &requests),
            Path::new("unused.log"),
            &env,
        )
        .pipe_err()
        .to_string();
        assert!(err.contains("cloud"), "{err}");
        assert!(err.contains("claude"), "{err}");
        drop(mock);
    }

    #[test]
    fn poll_transport_reset_holds_without_deleting() {
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let hits_t = Arc::clone(&hits);
        let stop_t = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let Ok((method, path, auth, body)) = read_http(&mut stream) else {
                    continue;
                };
                let reset = method == "GET" && path.contains("/sessions/");
                hits_t.lock().unwrap().push(Hit {
                    method,
                    path: path.clone(),
                    body,
                    auth,
                });
                if reset {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let payload = if path.contains("/repositories") {
                    json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]}).to_string()
                } else if path.ends_with("/sessions") {
                    created_body("devin-created")
                } else {
                    json!({"ok": true}).to_string()
                };
                let _ = write_http(&mut stream, 200, "OK", &payload);
            }
        });
        let adapter = DevinCloudAdapter::new(
            hooks(
                &Arc::new(Mutex::new(Vec::new())),
                &Arc::new(Mutex::new(Vec::new())),
            ),
            &env_at(&format!("http://{addr}")),
        );
        adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        let err = adapter
            .run_turn("do the task", "turn-1", &|_| {})
            .pipe_err();
        assert_eq!(err.kind(), "unknown");
        let text = err.to_string();
        assert!(text.contains("held last state"), "{text}");
        assert!(!text.contains(KEY), "transport error leaked the api key");
        assert!(!adapter.disconnected());
        let recorded = hits.lock().unwrap();
        assert!(recorded.iter().all(|hit| hit.method != "DELETE"));
        assert!(recorded
            .iter()
            .all(|hit| !hit.body.contains(KEY) && !hit.path.contains(KEY)));
        assert!(recorded
            .iter()
            .all(|hit| hit.auth == format!("Bearer {KEY}")));
        drop(recorded);
        stop.store(true, Ordering::SeqCst);
        let _ = thread.join();
    }

    #[test]
    fn bounded_cuts_on_a_char_boundary() {
        let text = format!("{}你tail", "a".repeat(179));
        let out = bounded(&text);
        assert!(out.contains('你'), "{out}");
        assert!(out.ends_with('…'), "{out}");
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        assert!(out.chars().count() <= 181, "{}", out.chars().count());
    }

    #[test]
    fn log_note_is_scrubbed_and_bounded() {
        let Harness {
            adapter,
            mock: _mock,
            ..
        } = adapter_for(Script::Happy);
        for _ in 0..400 {
            adapter.note(&format!("provider said {KEY}"));
        }
        let log = adapter.log.lock().unwrap().clone();
        assert!(log.contains("[redacted]"), "{log}");
        assert!(!log.contains(KEY), "{log}");
        assert!(log.len() <= LOG_LIMIT + 8, "{}", log.len());
    }

    #[test]
    fn later_turn_does_not_bind_an_earlier_sha() {
        let Harness {
            adapter,
            mock: _mock,
            ..
        } = adapter_for(Script::StaleSha);
        adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        let first = adapter.run_turn("revision one", "turn-1", &|_| {}).unwrap();
        assert!(first.text.contains(SHA), "{}", first.text);
        let second = adapter.run_turn("revision two", "turn-2", &|_| {}).unwrap();
        assert!(
            !second.text.contains(SHA),
            "revision 2 bound revision 1's SHA: {}",
            second.text
        );
        assert!(
            second.text.contains("revision two has no trailer"),
            "{}",
            second.text
        );
    }

    #[test]
    fn preflight_429_retries_and_does_not_reject() {
        let Harness { adapter, mock, .. } = adapter_for(Script::Preflight429);
        let ident = adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        assert_eq!(ident.session_id, "devin-created");
        let checks = mock
            .hits
            .lock()
            .unwrap()
            .iter()
            .filter(|hit| hit.path.contains("/repositories"))
            .count();
        assert!(checks >= 2, "expected a retried preflight, saw {checks}");
    }

    #[test]
    fn create_429_is_not_unknown() {
        let Harness {
            adapter,
            mock: _mock,
            ..
        } = adapter_for(Script::Create429);
        let ident = adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        assert_eq!(ident.session_id, "devin-created");
        let Harness {
            adapter,
            mock: _mock,
            ..
        } = adapter_for(Script::Create429Forever);
        let err = adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .pipe_err();
        assert_ne!(err.kind(), "unknown", "{err}");
        let text = err.to_string();
        assert!(text.contains("nothing created"), "{text}");
        assert!(!text.contains(KEY), "{text}");
    }

    #[test]
    fn create_timeout_adopts_the_owned_tag() {
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let hits_t = Arc::clone(&hits);
        let stop_t = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let Ok((method, path, auth, body)) = read_http(&mut stream) else {
                    continue;
                };
                let posts = {
                    let mut guard = hits_t.lock().unwrap();
                    guard.push(Hit {
                        method: method.clone(),
                        path: path.clone(),
                        body,
                        auth,
                    });
                    guard
                        .iter()
                        .filter(|hit| hit.method == "POST" && hit.path.ends_with("/sessions"))
                        .count()
                };
                if method == "POST" && path.ends_with("/sessions") && posts == 1 {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let claim = {
                    let guard = hits_t.lock().unwrap();
                    guard
                        .iter()
                        .rev()
                        .find(|hit| hit.method == "POST" && hit.path.ends_with("/sessions"))
                        .and_then(|hit| serde_json::from_str::<Value>(&hit.body).ok())
                        .and_then(|body| {
                            body.get("tags").and_then(Value::as_array).and_then(|tags| {
                                tags.iter().find_map(|tag| {
                                    tag.as_str()
                                        .filter(|text| text.starts_with("cadence-agent:"))
                                        .map(str::to_string)
                                })
                            })
                        })
                        .unwrap_or_default()
                };
                let payload = if path.contains("/repositories") {
                    json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]}).to_string()
                } else if path.contains("filter_tag=") {
                    json!({"sessions": [
                        {
                            "session_id": "devin-other",
                            "tags": ["cadence:daemon-1", "cadence-agent:other-worker:ffff"]
                        },
                        {
                            "session_id": "devin-recovered",
                            "tags": ["cadence:daemon-1", claim]
                        }
                    ]})
                    .to_string()
                } else if path.contains("/sessions/devin-other") {
                    session_body("devin-other", "working", "")
                } else if path.contains("/sessions/devin-recovered") {
                    session_body("devin-recovered", "working", "")
                } else {
                    created_body("devin-created")
                };
                let _ = write_http(&mut stream, 200, "OK", &payload);
            }
        });
        let adapter = DevinCloudAdapter::new(
            hooks(
                &Arc::new(Mutex::new(Vec::new())),
                &Arc::new(Mutex::new(Vec::new())),
            ),
            &env_at(&format!("http://{addr}")),
        );
        let ident = adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .unwrap();
        assert_eq!(ident.session_id, "devin-recovered");
        let recorded = hits.lock().unwrap();
        let create = recorded
            .iter()
            .find(|hit| hit.method == "POST" && hit.path.ends_with("/sessions"))
            .expect("create post");
        assert!(
            create.body.contains("cadence-agent:cloud-1:"),
            "create tag was not agent-specific: {}",
            create.body
        );
        let lookup = recorded
            .iter()
            .find(|hit| hit.path.contains("filter_tag="))
            .expect("owner lookup");
        assert!(
            lookup.path.contains("cadence-agent"),
            "lookup did not search the agent tag: {}",
            lookup.path
        );
        assert!(
            !lookup.path.contains("other-worker"),
            "lookup searched another agent's tag: {}",
            lookup.path
        );
        assert_eq!(
            recorded
                .iter()
                .filter(|hit| hit.method == "POST" && hit.path.ends_with("/sessions"))
                .count(),
            1,
            "a timed-out create must not start a second session"
        );
        assert!(recorded
            .iter()
            .all(|hit| !hit.path.contains("/sessions/devin-other")));
        drop(recorded);
        stop.store(true, Ordering::SeqCst);
        let _ = thread.join();
    }

    #[test]
    fn create_timeout_does_not_adopt_another_agents_session() {
        let stop = Arc::new(AtomicBool::new(false));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop_t = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let Ok((method, path, _, _)) = read_http(&mut stream) else {
                    continue;
                };
                if method == "POST" && path.ends_with("/sessions") {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let payload = if path.contains("/repositories") {
                    json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]}).to_string()
                } else if path.contains("filter_tag=") {
                    json!({"sessions": [{
                        "session_id": "devin-other",
                        "tags": ["cadence:daemon-1", "cadence-agent:other-worker:ffff"]
                    }]})
                    .to_string()
                } else if path.contains("/sessions/devin-other") {
                    session_body("devin-other", "working", "")
                } else {
                    created_body("devin-created")
                };
                let _ = write_http(&mut stream, 200, "OK", &payload);
            }
        });
        let adapter = DevinCloudAdapter::new(
            hooks(
                &Arc::new(Mutex::new(Vec::new())),
                &Arc::new(Mutex::new(Vec::new())),
            ),
            &env_at(&format!("http://{addr}")),
        );
        let err = adapter
            .open(&agent(json!({"repos": ["favcrm/cadence"]})))
            .pipe_err();
        assert_eq!(err.kind(), "provider", "{err}");
        let text = err.to_string();
        assert!(text.contains("no owned session"), "{text}");
        assert!(!text.contains("devin-other"), "{text}");
        stop.store(true, Ordering::SeqCst);
        let _ = thread.join();
    }

    fn read_http(stream: &mut TcpStream) -> std::io::Result<(String, String, String, String)> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            let n = stream.read(&mut tmp)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "eof",
                ));
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            if buf.len() > 64 * 1024 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "headers",
                ));
            }
        };
        let header = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let mut rest = buf[header_end + 4..].to_vec();
        let content_len = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        while rest.len() < content_len {
            let n = stream.read(&mut tmp)?;
            if n == 0 {
                break;
            }
            rest.extend_from_slice(&tmp[..n]);
        }
        let body = String::from_utf8_lossy(&rest[..content_len.min(rest.len())]).into_owned();
        let request = header.lines().next().unwrap_or("");
        let mut parts = request.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();
        let auth = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().to_string())
            })
            .unwrap_or_default();
        Ok((method, path, auth, body))
    }

    fn write_http(
        stream: &mut TcpStream,
        status: u16,
        reason: &str,
        body: &str,
    ) -> std::io::Result<()> {
        let payload = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(payload.as_bytes())
    }
}
