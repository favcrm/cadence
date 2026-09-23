//! Managed Codex adapter over stdio or a loopback WebSocket app-server
//! (`managed` / `managed_ws` endpoints).
//!
//! Ports the reference adapter: initialize + thread start/resume,
//! `turn/start` correlated by `clientUserMessageId`, turn completion via
//! `turn/completed`, final-answer `agentMessage` items, provider request
//! brokering, and `turn/interrupt`. The app-server interface is marked
//! experimental by its vendor; the tested CLI version is negotiated at
//! initialize, not assumed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::link::Incoming;
use super::registry;
use super::stdio::{EnvScrub, StdioAdapter};
use super::ws::WsAdapter;
use super::{AdapterHooks, Identity, ProviderAdapter, ProviderEnv, ProviderRequest, TurnResult};
use crate::error::{Error, Result};
use crate::store::Agent;

/// Silence bound before a turn is `unknown` (`params.turn_idle_secs`) —
/// the same activity-based liveness as managed claude: a turn that keeps
/// streaming is alive however long it runs. `params.turn_max_secs` adds
/// an optional absolute cap (CAD-227; the old fixed 600 s wall clock
/// fenced healthy long turns).
const DEFAULT_TURN_IDLE: Duration = Duration::from_secs(900);
/// Quota is advisory startup telemetry. A provider/auth mode that does not
/// expose the endpoint must not hold an agent open for a turn window.
const QUOTA_DEADLINE: Duration = Duration::from_secs(5);

const ENV_SCRUB: &[&str] = &[
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
];

pub(crate) fn scrub_names() -> Vec<&'static str> {
    let mut names = ENV_SCRUB.to_vec();
    names.extend_from_slice(super::CLOUD_SECRET_ENV);
    names
}

/// The provider's model catalogue is queried only when a caller requests a
/// model or effort. The app-server returns this metadata through
/// `model/list`; keeping the parser local means an unknown/changed wire shape
/// is surfaced as unknown availability instead of becoming a silent default.
#[derive(Debug)]
struct ModelMetadata {
    name: String,
    supported_efforts: Option<Vec<String>>,
    is_default: bool,
}

fn configured_settings(agent: &Agent) -> Result<(Option<String>, Option<String>)> {
    let params = agent.params.as_ref();
    let model = match params.and_then(|p| p.get("model")) {
        None => None,
        Some(Value::String(model)) if !model.trim().is_empty() => Some(model.clone()),
        Some(_) => return Err(Error::rejected("codex model must be a non-empty string")),
    };
    let effort = match params.and_then(|p| p.get("effort")) {
        None => None,
        Some(Value::String(effort)) => {
            registry::codex_effort(effort)?;
            Some(effort.clone())
        }
        Some(_) => {
            return Err(Error::rejected(format!(
                "codex effort must be a string, one of: {}",
                registry::CODEX_EFFORTS.join(", ")
            )))
        }
    };
    Ok((model, effort))
}

fn model_metadata(result: &Value) -> Result<Vec<ModelMetadata>> {
    let data = result
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::provider("Codex model availability unknown: model/list returned no data")
        })?;
    let mut models = Vec::with_capacity(data.len());
    for item in data {
        let name = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .or_else(|| {
                item.get("model")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
            })
            .ok_or_else(|| {
                Error::provider(
                    "Codex model availability unknown: model/list returned an entry without an id",
                )
            })?
            .to_string();
        let supported_efforts = item
            .get("supportedReasoningEfforts")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        item.as_str()
                            .or_else(|| item.get("reasoningEffort").and_then(Value::as_str))
                            .map(str::to_string)
                    })
                    .collect::<Vec<_>>()
            });
        models.push(ModelMetadata {
            name,
            supported_efforts,
            is_default: item
                .get("isDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(models)
}

/// Validate the requested `(model, effort)` pair against the provider's
/// advertised model catalogue. A catalogue failure is intentionally a
/// provider error labelled "availability unknown"; accepting the request
/// would otherwise silently inherit the user's global Codex model.
fn validate_settings(
    transport: &Transport,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<()> {
    if model.is_none() && effort.is_none() {
        return Ok(());
    }
    let mut models = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..32 {
        let mut request = json!({"includeHidden": true, "limit": 256});
        if let Some(value) = &cursor {
            request["cursor"] = json!(value);
        }
        let result = transport.request("model/list", request).map_err(|error| {
            Error::provider(format!(
                "Codex model availability unknown: model/list failed: {error}"
            ))
        })?;
        models.extend(model_metadata(&result)?);
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    if cursor.is_some() {
        return Err(Error::provider(
            "Codex model availability unknown: model/list pagination exceeded its bound",
        ));
    }
    let selected = match model {
        Some(name) => models.iter().find(|entry| entry.name == name),
        None => models.iter().find(|entry| entry.is_default),
    };
    let Some(selected) = selected else {
        if model.is_none() {
            return Err(Error::provider(
                "Codex model availability unknown: model/list did not identify a default model",
            ));
        }
        let requested = model.unwrap_or("provider default");
        return Err(Error::provider(format!(
            "Codex provider rejected model '{requested}': it is not present in model/list metadata"
        )));
    };
    if let Some(effort) = effort {
        let Some(supported) = selected.supported_efforts.as_ref() else {
            return Err(Error::provider(format!(
                "Codex effort availability unknown for model '{}': model/list omitted supportedReasoningEfforts",
                selected.name
            )));
        };
        if !supported.iter().any(|candidate| candidate == effort) {
            return Err(Error::provider(format!(
                "Codex provider rejected effort '{effort}' for model '{}': supported efforts are {}",
                selected.name,
                if supported.is_empty() {
                    "none".to_string()
                } else {
                    supported.join(", ")
                }
            )));
        }
    }
    Ok(())
}

/// Provider command; `CADENCE_CODEX_COMMAND` overrides it (test/mock use).
fn codex_command(env: &ProviderEnv) -> Vec<String> {
    if let Some(cmd) = env.var("CADENCE_CODEX_COMMAND") {
        let parts: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
        if !parts.is_empty() {
            return parts;
        }
    }
    ["codex", "app-server", "--listen", "stdio://"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Command prefix for the WebSocket app-server; `--listen <url>` is
/// appended by the transport. `CADENCE_CODEX_WS_COMMAND` overrides it.
fn codex_ws_command(env: &ProviderEnv) -> Vec<String> {
    if let Some(cmd) = env.var("CADENCE_CODEX_WS_COMMAND") {
        let parts: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
        if !parts.is_empty() {
            return parts;
        }
    }
    ["codex", "app-server"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Owned provider process + wire transport, selected by endpoint kind.
pub enum Transport {
    Stdio(Arc<StdioAdapter>),
    Ws(Arc<WsAdapter>),
}

/// A launched transport: provider pid plus its attachable endpoint.
pub struct Launched {
    pub pid: u32,
    pub endpoint: Option<String>,
}

impl Transport {
    fn launch(&self, cwd: &str, log: &Path) -> Result<Launched> {
        match self {
            Transport::Stdio(adapter) => Ok(Launched {
                pid: adapter.launch(cwd, log, &[])?,
                endpoint: None,
            }),
            Transport::Ws(adapter) => {
                let launched = adapter.launch(cwd, log)?;
                Ok(Launched {
                    pid: launched.pid,
                    endpoint: Some(launched.endpoint),
                })
            }
        }
    }

    fn send(&self, message: Value) -> Result<()> {
        match self {
            Transport::Stdio(a) => a.send(message),
            Transport::Ws(a) => a.send(message),
        }
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        match self {
            Transport::Stdio(a) => a.request(method, params),
            Transport::Ws(a) => a.request(method, params),
        }
    }

    fn request_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        match self {
            Transport::Stdio(a) => a.request_timeout(method, params, timeout),
            Transport::Ws(a) => a.request_timeout(method, params, timeout),
        }
    }

    fn respond(&self, request_id: &Value, result: Value) -> Result<()> {
        match self {
            Transport::Stdio(a) => a.respond(request_id, result),
            Transport::Ws(a) => a.respond(request_id, result),
        }
    }

    fn disconnected(&self) -> bool {
        match self {
            Transport::Stdio(a) => a.disconnected(),
            Transport::Ws(a) => a.disconnected(),
        }
    }

    fn close(&self) {
        match self {
            Transport::Stdio(a) => a.close(),
            Transport::Ws(a) => a.close(),
        }
    }

    /// True when an official Codex TUI can attach to this endpoint.
    fn attachable(&self) -> bool {
        matches!(self, Transport::Ws(_))
    }
}

pub struct CodexAdapter {
    transport: Transport,
    shared: Arc<Shared>,
    log_path: PathBuf,
}

/// `turnId → itemId → agentMessage item`, collected while a turn runs.
type TurnItems = HashMap<String, HashMap<String, Value>>;

struct Shared {
    hooks: AdapterHooks,
    items: Mutex<TurnItems>,
    completed: Mutex<HashMap<String, Value>>,
    turn_cv: Condvar,
    thread_id: Mutex<Option<String>>,
    active_turn: Mutex<Option<String>>,
    /// Every transport message bumps this — the raw activity clock the
    /// daemon's stall watch reads (`ProviderAdapter::activity_at`).
    last_activity: Mutex<Instant>,
    /// `params.turn_idle_secs` (default 900 s) and optional
    /// `params.turn_max_secs`, read at `open`.
    idle_window: Mutex<Duration>,
    max_turn: Mutex<Option<Duration>>,
    /// The last provider-owned rate-limit snapshot. It remains raw JSON;
    /// the store adds Cadence identity and timestamps before exposure.
    quota: Mutex<Option<Value>>,
}

fn shared_state(hooks: AdapterHooks) -> Arc<Shared> {
    Arc::new(Shared {
        hooks,
        items: Mutex::new(HashMap::new()),
        completed: Mutex::new(HashMap::new()),
        turn_cv: Condvar::new(),
        thread_id: Mutex::new(None),
        active_turn: Mutex::new(None),
        last_activity: Mutex::new(Instant::now()),
        idle_window: Mutex::new(DEFAULT_TURN_IDLE),
        max_turn: Mutex::new(None),
        quota: Mutex::new(None),
    })
}

impl CodexAdapter {
    /// Hooks wired into a fresh transport adapter.
    fn build_transport(command: &[String], ws: bool, shared: &Arc<Shared>) -> Transport {
        let routed = Arc::clone(shared);
        let disconnected = Arc::clone(shared);
        let names = scrub_names();
        if ws {
            Transport::Ws(WsAdapter::new(
                command,
                &names,
                Box::new(move |incoming| routed.dispatch(incoming)),
                Box::new(move || disconnected.on_disconnect()),
            ))
        } else {
            Transport::Stdio(StdioAdapter::new(
                command,
                EnvScrub::names(&names),
                Box::new(move |incoming| routed.dispatch(incoming)),
                Box::new(move || disconnected.on_disconnect()),
            ))
        }
    }

    /// stdio endpoint: `codex app-server --listen stdio://`.
    pub fn new(hooks: AdapterHooks, log_path: &Path, env: &ProviderEnv) -> Self {
        let shared = shared_state(hooks);
        Self {
            transport: Self::build_transport(&codex_command(env), false, &shared),
            shared,
            log_path: log_path.to_path_buf(),
        }
    }

    /// WebSocket endpoint: `codex app-server --listen ws://127.0.0.1:PORT`,
    /// attachable by an official TUI via `codex resume --remote`.
    pub fn new_ws(hooks: AdapterHooks, log_path: &Path, env: &ProviderEnv) -> Self {
        let shared = shared_state(hooks);
        Self {
            transport: Self::build_transport(&codex_ws_command(env), true, &shared),
            shared,
            log_path: log_path.to_path_buf(),
        }
    }

    /// Read the provider-owned allowance snapshot after the native thread is
    /// known. A missing endpoint/auth capability is recorded as unknown and
    /// never prevents the agent from starting.
    fn read_rate_limits(&self) {
        match self
            .transport
            .request_timeout("account/rateLimits/read", json!({}), QUOTA_DEADLINE)
        {
            Ok(data) => self
                .shared
                .record_quota("account/rateLimits/read", Some(data), None),
            Err(_) => self.shared.record_quota(
                "account/rateLimits/read",
                None,
                Some("Codex account/rateLimits/read unavailable"),
            ),
        }
    }

    /// One minimal turn so a fresh thread's rollout is persisted and an
    /// official TUI can attach to it. Bounded by its own deadline.
    fn seed_turn(&self, thread_id: &str) -> Result<()> {
        let result = self.transport.request_timeout(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{"type": "text",
                    "text": "Cadence endpoint initialization. Reply READY."}],
            }),
            Duration::from_secs(120),
        )?;
        let turn_id = result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::unknown("seed turn/start returned no turn id"))?
            .to_string();
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut completed = self.shared.completed.lock().unwrap();
        loop {
            if completed.remove(&turn_id).is_some() {
                return Ok(());
            }
            if self.transport.disconnected() {
                return Err(Error::unknown("Connection lost during seed turn"));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::unknown("Seed turn deadline reached"));
            }
            let (guard, _) = self
                .shared
                .turn_cv
                .wait_timeout(completed, remaining)
                .unwrap();
            completed = guard;
        }
    }
}

impl Shared {
    /// Merge a sparse provider update. Omitted fields retain their last
    /// confirmed value, while Codex's nullable window fields explicitly
    /// replace stale telemetry with a JSON null. Other nullable fields,
    /// including account identity, remain conservative and retain the last
    /// confirmed value.
    fn merge_non_null(target: &mut Value, patch: &Value) {
        match (target, patch) {
            (Value::Object(target), Value::Object(patch)) => {
                for (key, value) in patch {
                    if value.is_null() {
                        if matches!(key.as_str(), "resetsAt" | "windowDurationMins") {
                            target.insert(key.clone(), Value::Null);
                        }
                        continue;
                    }
                    match target.get_mut(key) {
                        Some(existing) if existing.is_object() && value.is_object() => {
                            Self::merge_non_null(existing, value);
                        }
                        _ => {
                            target.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            (target, patch) if !patch.is_null() => *target = patch.clone(),
            _ => {}
        }
    }

    fn record_quota(&self, source: &str, data: Option<Value>, reason: Option<&str>) {
        let mut snapshot = json!({
            "state": if data.is_some() {
                "reported"
            } else if reason.is_some() {
                "unavailable"
            } else {
                "unknown"
            },
            "source": source,
        });
        if let Some(data) = data {
            snapshot["data"] = data;
        }
        if let Some(reason) = reason {
            snapshot["reason"] = json!(reason);
        }
        *self.quota.lock().unwrap() = Some(snapshot);
    }

    fn merge_quota_update(&self, source: &str, patch: &Value) -> Value {
        let mut quota = self.quota.lock().unwrap();
        let snapshot =
            quota.get_or_insert_with(|| json!({"state": "unknown", "source": source, "data": {}}));
        if !snapshot.get("data").is_some_and(Value::is_object) {
            snapshot["data"] = json!({});
        }
        let data = snapshot.get_mut("data").expect("quota data inserted");
        Self::merge_non_null(data, patch);
        snapshot["state"] = json!("reported");
        snapshot["source"] = json!(source);
        snapshot["data"].clone()
    }

    fn quota_snapshot(&self) -> Option<Value> {
        self.quota.lock().unwrap().clone()
    }

    fn dispatch(&self, incoming: Incoming) {
        *self.last_activity.lock().unwrap() = Instant::now();
        match incoming {
            Incoming::Request { id, method, params } => {
                (self.hooks.on_request)(ProviderRequest { id, method, params });
            }
            Incoming::Notification { method, params } => self.notification(&method, params),
        }
    }

    fn notification(&self, method: &str, params: Value) {
        match method {
            "item/completed" => {
                let item = &params["item"];
                if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
                    if let (Some(turn), Some(id)) = (
                        params.get("turnId").and_then(Value::as_str),
                        item.get("id").and_then(Value::as_str),
                    ) {
                        self.items
                            .lock()
                            .unwrap()
                            .entry(turn.to_string())
                            .or_default()
                            .insert(id.to_string(), item.clone());
                    }
                    self.emit("item/completed", &params);
                }
            }
            "turn/completed" => {
                if let Some(turn_id) = params.pointer("/turn/id").and_then(Value::as_str) {
                    self.completed
                        .lock()
                        .unwrap()
                        .insert(turn_id.to_string(), params["turn"].clone());
                    self.turn_cv.notify_all();
                }
                self.emit(method, &params);
            }
            "account/rateLimits/updated" => {
                let data = self.merge_quota_update(method, &params);
                let thread_id = self.thread_id.lock().unwrap().clone();
                self.emit(
                    "cadence/codex_quota",
                    &json!({
                        "thread_id": thread_id,
                        "source": method,
                        "data": data,
                    }),
                );
            }
            "turn/started" | "error" | "serverRequest/resolved" => self.emit(method, &params),
            _ => {}
        }
    }

    fn emit(&self, method: &str, params: &Value) {
        (self.hooks.on_event)(method, params.clone());
    }

    /// Transport EOF: wake any turn-completion wait so it can re-check
    /// `disconnected` instead of sleeping out the turn deadline. The
    /// `completed` lock serializes against the waiter's check-then-sleep.
    fn on_disconnect(&self) {
        let _guard = self.completed.lock().unwrap();
        self.turn_cv.notify_all();
    }
}

impl ProviderAdapter for CodexAdapter {
    /// The raw transport clock — `Shared::dispatch` stamps every
    /// incoming message, so the daemon's stall watch reads true
    /// provider traffic, not only the curated event stream.
    fn activity_at(&self) -> Option<Instant> {
        Some(*self.shared.last_activity.lock().unwrap())
    }

    fn quota_snapshot(&self) -> Option<Value> {
        self.shared.quota_snapshot()
    }

    fn open(&self, agent: &Agent) -> Result<Identity> {
        let window = |key: &str| {
            agent
                .params
                .as_ref()
                .and_then(|p| p.get(key))
                .and_then(Value::as_u64)
                .map(|s| Duration::from_secs(s.max(1)))
        };
        *self.shared.idle_window.lock().unwrap() =
            window("turn_idle_secs").unwrap_or(DEFAULT_TURN_IDLE);
        *self.shared.max_turn.lock().unwrap() = window("turn_max_secs");
        let launched = self.transport.launch(&agent.cwd, &self.log_path)?;
        // Everything after launch is guarded: any failure closes the
        // transport so no owned provider process is left behind.
        let opened = (|| -> Result<Identity> {
            self.transport.request(
                "initialize",
                json!({"clientInfo": {"name": "cadence-agent", "version": "0.1.0"}}),
            )?;
            self.transport
                .send(json!({"method": "initialized", "params": {}}))?;
            // `approval_policy` rides params so resume replays it
            // verbatim; a cadence-launched worker defaults to `never` —
            // the same unattended posture the other providers run.
            // Registration and `agent set --next-launch` already reject
            // unknown values; validating again here keeps a hand-edited
            // store from reaching the wire silently.
            let approval_policy = match agent.params.as_ref().and_then(|p| p.get("approval_policy"))
            {
                Some(v) => {
                    let policy = v.as_str().ok_or_else(|| {
                        Error::rejected(format!(
                            "codex approval_policy must be a string, one of: {}",
                            registry::CODEX_APPROVAL_POLICIES.join(", ")
                        ))
                    })?;
                    registry::codex_approval_policy(policy)?;
                    policy
                }
                None => "never",
            };
            let (configured_model, configured_effort) = configured_settings(agent)?;
            validate_settings(
                &self.transport,
                configured_model.as_deref(),
                configured_effort.as_deref(),
            )?;
            let mut params = json!({
                "cwd": agent.cwd,
                "sandbox": agent.sandbox,
                "approvalPolicy": approval_policy,
            });
            if let Some(model) = configured_model.as_deref() {
                params["model"] = json!(model);
            }
            if let Some(effort) = configured_effort.as_deref() {
                // Scope the reasoning setting to this managed thread. This
                // uses the app-server config field without touching the
                // operator's global ~/.codex configuration.
                params["config"] = json!({"model_reasoning_effort": effort});
            }
            if let Some(instructions) = &agent.instructions {
                params["developerInstructions"] = json!(instructions);
            }
            if let Some(thread) = &agent.thread_id {
                params["threadId"] = json!(thread);
            }
            let fresh = agent.thread_id.is_none();
            let method = if fresh {
                "thread/start"
            } else {
                "thread/resume"
            };
            let result = self.transport.request(method, params)?;
            let thread_id = result
                .pointer("/thread/id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::provider("thread/start returned no thread id"))?
                .to_string();
            *self.shared.thread_id.lock().unwrap() = Some(thread_id.clone());
            let effective_model = result
                .get("model")
                .or_else(|| result.pointer("/thread/model"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let effective_effort = result
                .get("reasoningEffort")
                .or_else(|| result.pointer("/thread/reasoningEffort"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if let (Some(requested), Some(effective)) =
                (configured_model.as_deref(), effective_model.as_deref())
            {
                if requested != effective {
                    return Err(Error::provider(format!(
                        "Codex provider selected model '{effective}' instead of requested '{requested}'; refusing silent substitution"
                    )));
                }
            } else if configured_model.is_some() {
                return Err(Error::provider(
                    "Codex effective model is unknown: thread response omitted model",
                ));
            }
            if let (Some(requested), Some(effective)) =
                (configured_effort.as_deref(), effective_effort.as_deref())
            {
                if requested != effective {
                    return Err(Error::provider(format!(
                        "Codex provider selected effort '{effective}' instead of requested '{requested}'; refusing silent substitution"
                    )));
                }
            } else if configured_effort.is_some() {
                return Err(Error::provider(
                    "Codex effective effort is unknown: thread response omitted reasoningEffort",
                ));
            }
            self.read_rate_limits();
            // A TUI can only resume a thread whose rollout is persisted —
            // which happens after its first turn. Seed fresh WebSocket
            // threads with one minimal turn so `agent attach` works.
            if fresh && self.transport.attachable() {
                self.seed_turn(&thread_id)?;
            }
            Ok(Identity {
                session_id: result
                    .pointer("/thread/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or(&thread_id)
                    .to_string(),
                thread_id,
                model: effective_model,
                effort: effective_effort,
                pid: launched.pid,
                endpoint: launched.endpoint.clone(),
                generation: None,
                attach: None,
            })
        })();
        opened.inspect_err(|_| self.transport.close())
    }

    fn run_turn(
        &self,
        prompt: &str,
        client_message_id: &str,
        on_started: &dyn Fn(&str),
    ) -> Result<TurnResult> {
        let thread_id = self
            .shared
            .thread_id
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::provider("Codex thread is not open"))?;
        let result = self.transport.request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{"type": "text", "text": prompt}],
                "clientUserMessageId": client_message_id,
            }),
        )?;
        let turn_id = result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            // The provider acknowledged the request but we cannot correlate
            // a turn — execution may have started, so this is not a
            // definitive failure.
            .ok_or_else(|| Error::unknown("turn/start acknowledged but returned no turn id"))?
            .to_string();
        *self.shared.active_turn.lock().unwrap() = Some(turn_id.clone());
        on_started(&turn_id);
        let start = Instant::now();
        *self.shared.last_activity.lock().unwrap() = start;
        let idle_window = *self.shared.idle_window.lock().unwrap();
        let max_turn = *self.shared.max_turn.lock().unwrap();
        let turn = {
            let mut completed = self.shared.completed.lock().unwrap();
            loop {
                if let Some(turn) = completed.remove(&turn_id) {
                    break turn;
                }
                if self.disconnected() {
                    return Err(Error::unknown(
                        "Connection lost during turn; provider outcome is unknown",
                    ));
                }
                let now = Instant::now();
                let last = *self.shared.last_activity.lock().unwrap();
                let idle_left = (last + idle_window).saturating_duration_since(now);
                if idle_left.is_zero() {
                    return Err(Error::unknown(format!(
                        "No provider event for {}s; outcome is unknown",
                        idle_window.as_secs()
                    )));
                }
                let max_left = max_turn.map(|cap| (start + cap).saturating_duration_since(now));
                if matches!(max_left, Some(d) if d.is_zero()) {
                    return Err(Error::unknown(format!(
                        "Turn exceeded turn_max_secs ({}s); provider outcome needs review",
                        max_turn.unwrap_or_default().as_secs()
                    )));
                }
                let remaining = max_left.map_or(idle_left, |m| m.min(idle_left));
                let (guard, _) = self
                    .shared
                    .turn_cv
                    .wait_timeout(completed, remaining)
                    .unwrap();
                completed = guard;
            }
        };
        *self.shared.active_turn.lock().unwrap() = None;
        let mut items = self
            .shared
            .items
            .lock()
            .unwrap()
            .remove(&turn_id)
            .unwrap_or_default();
        if let Some(list) = turn.get("items").and_then(Value::as_array) {
            for item in list {
                if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
                    if let Some(id) = item.get("id").and_then(Value::as_str) {
                        items.insert(id.to_string(), item.clone());
                    }
                }
            }
        }
        let mut messages: Vec<&Value> = items.values().collect();
        messages.sort_by_key(|item| item.get("id").and_then(Value::as_str).unwrap_or(""));
        let finals: Vec<&Value> = messages
            .iter()
            .filter(|item| item.get("phase").and_then(Value::as_str) == Some("final_answer"))
            .copied()
            .collect();
        let selected = if finals.is_empty() { messages } else { finals };
        let text = selected
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(TurnResult {
            turn_id: turn
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(&turn_id)
                .to_string(),
            status: turn
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            text,
            stop_reason: None,
            error: turn
                .get("error")
                .filter(|e| !e.is_null())
                .map(|e| e.to_string()),
        })
    }

    fn respond(&self, request_id: &Value, result: Value) -> Result<()> {
        self.transport.respond(request_id, result)
    }

    fn interrupt(&self) {
        let thread_id = self.shared.thread_id.lock().unwrap().clone();
        let turn = self.shared.active_turn.lock().unwrap().clone();
        if let (Some(thread), Some(turn)) = (thread_id, turn) {
            if !self.disconnected() {
                let _ = self.transport.request_timeout(
                    "turn/interrupt",
                    json!({"threadId": thread, "turnId": turn}),
                    Duration::from_secs(5),
                );
            }
        }
    }

    fn disconnected(&self) -> bool {
        self.transport.disconnected()
    }

    fn close(&self) {
        self.transport.close();
    }
}
