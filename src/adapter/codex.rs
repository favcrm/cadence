//! Managed Codex adapter: `codex app-server --listen stdio://`.
//!
//! Ports the reference adapter: initialize + thread start/resume,
//! `turn/start` correlated by `clientUserMessageId`, turn completion via
//! `turn/completed`, final-answer `agentMessage` items, provider request
//! brokering, and `turn/interrupt`. The app-server interface is marked
//! experimental by its vendor; the tested CLI version is negotiated at
//! initialize, not assumed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::stdio::{Incoming, StdioAdapter};
use super::{AdapterHooks, Identity, ProviderAdapter, ProviderRequest, TurnResult};
use crate::error::{Error, Result};
use crate::store::Agent;

const TURN_DEADLINE: Duration = Duration::from_secs(600);

/// Provider command; `CADENCE_CODEX_COMMAND` overrides it (test/mock use).
fn codex_command() -> Vec<String> {
    if let Ok(cmd) = std::env::var("CADENCE_CODEX_COMMAND") {
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

pub struct CodexAdapter {
    stdio: Arc<StdioAdapter>,
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
}

impl CodexAdapter {
    pub fn new(hooks: AdapterHooks, log_path: &std::path::Path) -> Self {
        let shared = Arc::new(Shared {
            hooks,
            items: Mutex::new(HashMap::new()),
            completed: Mutex::new(HashMap::new()),
            turn_cv: Condvar::new(),
            thread_id: Mutex::new(None),
            active_turn: Mutex::new(None),
        });
        let routed = Arc::clone(&shared);
        let disconnected = Arc::clone(&shared);
        let stdio = StdioAdapter::new(
            &codex_command(),
            &[
                "CODEX_THREAD_ID",
                "CODEX_SESSION_ID",
                "CLAUDE_CODE_SESSION_ID",
            ],
            Box::new(move |incoming| routed.dispatch(incoming)),
            Box::new(move || disconnected.on_disconnect()),
        );
        Self {
            stdio,
            shared,
            log_path: log_path.to_path_buf(),
        }
    }
}

impl Shared {
    fn dispatch(&self, incoming: Incoming) {
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
            "turn/started" | "error" => self.emit(method, &params),
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
    fn open(&self, agent: &Agent) -> Result<Identity> {
        self.stdio.launch(&agent.cwd, &self.log_path)?;
        // Everything after launch is guarded: any failure closes the
        // transport so no owned provider process is left behind.
        let opened = (|| -> Result<Identity> {
            let pid = self.stdio.pid().unwrap_or(0);
            self.stdio.request(
                "initialize",
                json!({"clientInfo": {"name": "cadence-agent", "version": "0.1.0"}}),
            )?;
            self.stdio
                .send(json!({"method": "initialized", "params": {}}))?;
            let mut params = json!({
                "cwd": agent.cwd,
                "sandbox": agent.sandbox,
                "approvalPolicy": "on-request",
            });
            if let Some(instructions) = &agent.instructions {
                params["developerInstructions"] = json!(instructions);
            }
            if let Some(thread) = &agent.thread_id {
                params["threadId"] = json!(thread);
            }
            let method = if agent.thread_id.is_some() {
                "thread/resume"
            } else {
                "thread/start"
            };
            let result = self.stdio.request(method, params)?;
            let thread_id = result
                .pointer("/thread/id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::provider("thread/start returned no thread id"))?
                .to_string();
            *self.shared.thread_id.lock().unwrap() = Some(thread_id.clone());
            Ok(Identity {
                session_id: result
                    .pointer("/thread/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or(&thread_id)
                    .to_string(),
                thread_id,
                model: result
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                pid,
            })
        })();
        opened.inspect_err(|_| self.stdio.close())
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
        let result = self.stdio.request(
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
        let deadline = Instant::now() + TURN_DEADLINE;
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
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(Error::unknown(
                        "Turn deadline reached; provider outcome needs review",
                    ));
                }
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
        self.stdio.respond(request_id, result)
    }

    fn interrupt(&self) {
        let thread_id = self.shared.thread_id.lock().unwrap().clone();
        let turn = self.shared.active_turn.lock().unwrap().clone();
        if let (Some(thread), Some(turn)) = (thread_id, turn) {
            if !self.disconnected() {
                let _ = self.stdio.request_timeout(
                    "turn/interrupt",
                    json!({"threadId": thread, "turnId": turn}),
                    Duration::from_secs(5),
                );
            }
        }
    }

    fn disconnected(&self) -> bool {
        self.stdio.disconnected()
    }

    fn close(&self) {
        self.stdio.close();
    }
}
