//! Managed Claude adapter over the headless `stream-json` protocol
//! (`managed` endpoint, provider `claude`).
//!
//! Claude Code's long-lived headless mode —
//! `claude -p --input-format stream-json --output-format stream-json
//! --verbose` — reads one `{"type":"user","message":…}` object per line
//! on stdin and emits typed events (`system/init`, `assistant`, `result`,
//! …) on stdout. It is NOT JSON-RPC: there is no request id, so one
//! durable Cadence message maps to the next `result` event. Turns are
//! serialized by the actor, so only one turn is ever in flight.
//!
//! Semantics mapped to Cadence:
//! - `result.subtype == "success" && !is_error` → completed; the
//!   `result` field is the report text — no `message result` call exists.
//! - `is_error` or `subtype == "error_*"` → failed (a definitive answer).
//! - `subtype == "interrupted"` → interrupted.
//! - Process death or EOF before a `result` → `OutcomeUnknown`.
//! - `system/init` must report the session id this process was opened
//!   with (`--session-id` fresh, `--resume` on reopen); a mismatch means
//!   another process owns the expected session — the agent fences.
//! - Interrupt is SIGINT to the child's own process group; a bounded
//!   grace waits for the interrupted result, then fails closed.
//! - No approval brokering in phase A: `--permission-mode` (default
//!   `manual`) and `--allowedTools` (always `Bash(cadence *)` plus
//!   `params.allowed_tools`) are fixed at launch and replayed on resume.
//!   Denials surface as `permission_denials` on a still-successful
//!   result — recorded as an event, never a failure.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use uuid::Uuid;

use super::link::Incoming;
use super::stdio::StdioAdapter;
use super::{AdapterHooks, Identity, ProviderAdapter, TurnResult};
use crate::error::{Error, Result};
use crate::store::Agent;

const TURN_DEADLINE: Duration = Duration::from_secs(600);
/// After SIGINT the provider is expected to emit a final `result` —
/// a bounded grace keeps a hung interrupt from parking the actor.
const INTERRUPT_GRACE: Duration = Duration::from_secs(60);

/// Identity variables a child must never inherit from a parent
/// conversation (Claude session markers, foreign providers, and the
/// Cadence identity re-injected per-agent at launch).
const ENV_SCRUB: &[&str] = &[
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CADENCE_ALIAS",
    "CADENCE_STATE_DIR",
];

/// Provider binary; `CADENCE_CLAUDE_COMMAND` overrides it (test/mock).
/// Stream-json flags are appended after this prefix so a mock sees the
/// same argv shape (`--resume`, `--permission-mode`, …) as the real CLI.
fn claude_command() -> Vec<String> {
    if let Ok(cmd) = std::env::var("CADENCE_CLAUDE_COMMAND") {
        let parts: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
        if !parts.is_empty() {
            return parts;
        }
    }
    vec!["claude".to_string()]
}

/// `claude -p --input-format stream-json …` plus session/permission
/// flags. Stored `params.permission_mode` (default `manual`),
/// `params.allowed_tools` (added to the `Bash(cadence *)` baseline) and
/// `params.model` are replayed verbatim on every resume — the launch
/// line is rebuilt from the durable params, never from memory.
fn build_command(agent: &Agent, session_id: &str, resume: bool) -> Vec<String> {
    let params = agent.params.clone().unwrap_or(Value::Null);
    let mut cmd = claude_command();
    for flag in [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
    ] {
        cmd.push(flag.to_string());
    }
    if resume {
        cmd.extend(["--resume".to_string(), session_id.to_string()]);
    } else {
        cmd.extend(["--session-id".to_string(), session_id.to_string()]);
    }
    if let Some(model) = params
        .get("model")
        .and_then(Value::as_str)
        .or(agent.model.as_deref())
    {
        cmd.extend(["--model".to_string(), model.to_string()]);
    }
    let mode = params
        .get("permission_mode")
        .and_then(Value::as_str)
        .unwrap_or("manual");
    cmd.extend(["--permission-mode".to_string(), mode.to_string()]);
    let mut allowed: Vec<String> = vec!["Bash(cadence *)".to_string()];
    if let Some(list) = params.get("allowed_tools").and_then(Value::as_array) {
        allowed.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
    }
    for tool in allowed {
        cmd.extend(["--allowedTools".to_string(), tool]);
    }
    cmd
}

pub struct ClaudeAdapter {
    /// Swapped at `open()` — the real command line carries the session
    /// flags (`--session-id`/`--resume`, model, permissions) that only
    /// exist once the agent row is read.
    transport: RwLock<Arc<StdioAdapter>>,
    shared: Arc<Shared>,
    log_path: PathBuf,
    state_dir: PathBuf,
}

struct Shared {
    hooks: AdapterHooks,
    /// Result events not yet consumed by a `run_turn` waiter.
    results: Mutex<VecDeque<Value>>,
    result_cv: Condvar,
    /// Session id this adapter opened the process with.
    expected_session: Mutex<Option<String>>,
    /// Set when `system/init` reports a different session than opened.
    session_mismatch: Mutex<Option<String>>,
    /// Minted per `open`; prefixes this endpoint generation's turn ids.
    generation: Mutex<String>,
    /// Set by `interrupt()` — the next result's grace deadline.
    interrupt_at: Mutex<Option<Instant>>,
    dead: AtomicBool,
}

impl ClaudeAdapter {
    pub fn new(hooks: AdapterHooks, log_path: &Path) -> Self {
        let shared = Arc::new(Shared {
            hooks,
            results: Mutex::new(VecDeque::new()),
            result_cv: Condvar::new(),
            expected_session: Mutex::new(None),
            session_mismatch: Mutex::new(None),
            generation: Mutex::new(String::new()),
            interrupt_at: Mutex::new(None),
            dead: AtomicBool::new(false),
        });
        let routed = Arc::clone(&shared);
        let disconnected = Arc::clone(&shared);
        Self {
            transport: RwLock::new(StdioAdapter::new_lines(
                &claude_command(),
                ENV_SCRUB,
                Box::new(move |incoming| routed.dispatch(incoming)),
                Box::new(move || disconnected.on_disconnect()),
            )),
            shared,
            log_path: log_path.to_path_buf(),
            state_dir: log_path
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")),
        }
    }

    /// A transport bound to `command` but wired into this adapter's
    /// shared state — the swap target at `open()`.
    fn transport_for(&self, command: &[String]) -> Arc<StdioAdapter> {
        let routed = Arc::clone(&self.shared);
        let disconnected = Arc::clone(&self.shared);
        StdioAdapter::new_lines(
            command,
            ENV_SCRUB,
            Box::new(move |incoming| routed.dispatch(incoming)),
            Box::new(move || disconnected.on_disconnect()),
        )
    }
}

impl Shared {
    fn dispatch(&self, incoming: Incoming) {
        let Incoming::Notification { method, params } = incoming else {
            // stream-json has no server→client requests.
            return;
        };
        match method.as_str() {
            "system" if params.get("subtype").and_then(Value::as_str) == Some("init") => {
                self.on_init(&params);
            }
            "result" => {
                self.results.lock().unwrap().push_back(params.clone());
                self.result_cv.notify_all();
                self.emit_result_meta(&params);
            }
            _ => {}
        }
    }

    /// The session id in `system/init` is authoritative identity proof:
    /// anything but the id this process was opened with means another
    /// Claude owns the expected session — fail closed into `attention`.
    fn on_init(&self, event: &Value) {
        let observed = event.get("session_id").and_then(Value::as_str);
        let expected = self.expected_session.lock().unwrap().clone();
        if let (Some(want), Some(got)) = (expected, observed) {
            if got != want {
                *self.session_mismatch.lock().unwrap() = Some(format!(
                    "Claude session mismatch: init reported {got}, expected {want} — \
                     another Claude process owns session {want}; remove and rejoin \
                     to mint a fresh session"
                ));
                self.result_cv.notify_all();
            }
        }
    }

    fn emit_result_meta(&self, result: &Value) {
        let meta = json!({
            "session_id": result.get("session_id"),
            "subtype": result.get("subtype"),
            "is_error": result.get("is_error"),
            "stop_reason": result.get("stop_reason"),
            "num_turns": result.get("num_turns"),
            "duration_ms": result.get("duration_ms"),
            "total_cost_usd": result.get("total_cost_usd"),
        });
        self.emit("cadence/claude_result", &meta);
        if let Some(denials) = result
            .get("permission_denials")
            .and_then(Value::as_array)
            .filter(|d| !d.is_empty())
        {
            self.emit("cadence/permission_denied", &json!({ "denials": denials }));
        }
    }

    fn emit(&self, method: &str, params: &Value) {
        (self.hooks.on_event)(method, params.clone());
    }

    /// Transport EOF: wake any turn wait so it can re-check `dead`
    /// instead of sleeping out the turn deadline.
    fn on_disconnect(&self) {
        self.dead.store(true, Ordering::SeqCst);
        let _guard = self.results.lock().unwrap();
        self.result_cv.notify_all();
    }
}

impl ProviderAdapter for ClaudeAdapter {
    /// Open: fresh agents mint a `--session-id`; a stored `thread_id`
    /// reopens with `--resume`. The id is verified for real at the
    /// first `system/init` — Claude emits it lazily with the first
    /// turn, not at spawn.
    fn open(&self, agent: &Agent) -> Result<Identity> {
        let resume = agent.thread_id.is_some();
        let session_id = agent
            .thread_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let command = build_command(agent, &session_id, resume);
        *self.shared.expected_session.lock().unwrap() = Some(session_id.clone());
        *self.shared.session_mismatch.lock().unwrap() = None;
        let generation = Uuid::new_v4().simple().to_string()[..12].to_string();
        *self.shared.generation.lock().unwrap() = generation.clone();
        self.shared.dead.store(false, Ordering::SeqCst);
        let env = [
            ("CADENCE_ALIAS".to_string(), agent.alias.clone()),
            (
                "CADENCE_STATE_DIR".to_string(),
                self.state_dir.to_string_lossy().to_string(),
            ),
        ];
        let transport = self.transport_for(&command);
        let pid = transport.launch(&agent.cwd, &self.log_path, &env)?;
        *self.transport.write().unwrap() = transport;
        Ok(Identity {
            thread_id: session_id.clone(),
            session_id,
            model: agent.model.clone(),
            pid,
            endpoint: None,
            generation: Some(generation),
        })
    }

    /// One durable message → one `user` line → the next `result` event.
    /// The write itself is the only acknowledgement the wire offers;
    /// once it lands, a lost process is `OutcomeUnknown`, not a retry.
    fn run_turn(
        &self,
        prompt: &str,
        _client_message_id: &str,
        on_started: &dyn Fn(&str),
    ) -> Result<TurnResult> {
        let generation = self.shared.generation.lock().unwrap().clone();
        let turn_id = format!("claude-{generation}-{}", Uuid::new_v4().simple());
        // A queued result here is stale — it belongs to a turn the
        // daemon already fenced. Discard it rather than misattribute.
        {
            let mut queue = self.shared.results.lock().unwrap();
            while let Some(stale) = queue.pop_front() {
                self.shared
                    .emit("cadence/stale_result", &json!({"result": stale}));
            }
        }
        self.transport.read().unwrap().send(json!({
            "type": "user",
            "message": {"role": "user", "content": prompt},
        }))?;
        on_started(&turn_id);
        let deadline = Instant::now() + TURN_DEADLINE;
        let result = {
            let mut queue = self.shared.results.lock().unwrap();
            loop {
                if let Some(mismatch) = self.shared.session_mismatch.lock().unwrap().clone() {
                    return Err(Error::provider(mismatch));
                }
                if let Some(result) = queue.pop_front() {
                    break result;
                }
                if self.shared.dead.load(Ordering::SeqCst) {
                    return Err(Error::unknown(
                        "Claude process exited before a result; outcome is unknown",
                    ));
                }
                let interrupt_deadline = self
                    .shared
                    .interrupt_at
                    .lock()
                    .unwrap()
                    .map(|at| at + INTERRUPT_GRACE);
                if let Some(at) = interrupt_deadline {
                    if Instant::now() >= at {
                        return Err(Error::unknown(
                            "No result after interrupt; provider outcome is unknown",
                        ));
                    }
                }
                let remaining = deadline.saturating_duration_since(Instant::now()).min(
                    interrupt_deadline
                        .map(|at| at.saturating_duration_since(Instant::now()))
                        .unwrap_or(TURN_DEADLINE),
                );
                if remaining.is_zero() {
                    return Err(Error::unknown(
                        "Turn deadline reached; provider outcome needs review",
                    ));
                }
                let (guard, _) = self
                    .shared
                    .result_cv
                    .wait_timeout(queue, remaining.min(Duration::from_millis(250)))
                    .unwrap();
                queue = guard;
            }
        };
        *self.shared.interrupt_at.lock().unwrap() = None;
        let is_error = result
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let subtype = result
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let text = result
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let status = if subtype == "interrupted" || subtype.contains("interrupt") {
            "interrupted"
        } else if is_error || subtype.starts_with("error") {
            "failed"
        } else if subtype == "success" || subtype.is_empty() {
            "completed"
        } else {
            // An unrecognized terminal subtype is still a definitive
            // provider answer — fail the message, never fence on it.
            "failed"
        };
        // Error results carry an `errors` array, not `result` text —
        // surface the provider's own message before the subtype label.
        let errors = result
            .get("errors")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        let error = (status != "completed").then(|| {
            if !text.is_empty() {
                text.clone()
            } else if !errors.is_empty() {
                errors
            } else {
                format!("Claude result subtype '{subtype}' (is_error={is_error})")
            }
        });
        Ok(TurnResult {
            turn_id,
            status: status.to_string(),
            text,
            stop_reason: result
                .get("stop_reason")
                .and_then(Value::as_str)
                .map(str::to_string),
            error,
        })
    }

    /// stream-json brokers no provider→client requests in phase A.
    fn respond(&self, _request_id: &Value, _result: Value) -> Result<()> {
        Err(Error::rejected(
            "managed claude endpoints broker no requests — approval flow \
             opt-ups: --permission-prompt-tool, --include-hook-events, \
             or an MCP approval tool",
        ))
    }

    /// SIGINT the provider's own process group — Claude ends the active
    /// turn and emits a final `result` (interrupted). The waiter bounds
    /// the hang to `INTERRUPT_GRACE` before failing closed.
    fn interrupt(&self) {
        *self.shared.interrupt_at.lock().unwrap() = Some(Instant::now());
        self.transport.read().unwrap().interrupt();
    }

    fn disconnected(&self) -> bool {
        self.transport.read().unwrap().disconnected()
    }

    /// Close stdin first — stream-json treats EOF as a clean shutdown —
    /// then the transport's TERM→KILL sequence covers a stubborn child.
    fn close(&self) {
        let transport = self.transport.read().unwrap().clone();
        transport.close_stdin();
        transport.wait_exit(Duration::from_secs(3));
        transport.close();
    }
}
