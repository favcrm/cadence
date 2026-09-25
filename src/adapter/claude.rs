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
//! - Interrupt is the CLI's own stream-json control request
//!   (`{"type":"control_request","request":{"subtype":"interrupt"}}` on
//!   stdin, CAD-323) — never a signal or a kill; SIGINT to the child's
//!   process group is only the fallback when stdin is already gone. A
//!   bounded grace waits for the turn's final result, then fails closed.
//!   The interrupted turn's result is an error result whose
//!   `terminal_reason` is `aborted_streaming`/`aborted_tools` — mapped to
//!   `interrupted`, as is any error result of a turn Cadence interrupted.
//! - Turn liveness is activity, not wall clock: any stdout event resets
//!   the clock, and a turn fences `unknown` only after
//!   `params.turn_idle_secs` of silence (default 900) or the optional
//!   `params.turn_max_secs` absolute cap — a healthy multi-hour turn is
//!   never fenced for being long.
//! - One `tool_use` lifecycle event per assistant tool call (name plus a
//!   one-line redacted input summary, never the raw input)
//!   keeps `events --follow` meaningful without proxying the transcript.
//!   Each `tool_result` becomes one `tool_result` event (a redacted
//!   ≤160-char summary plus `is_error`, never the raw output), and each
//!   assistant text block an `assistant_text` event for a threaded
//!   agent's chat (CAD-320). The last text block is held until the next
//!   event: the one the `result` repeats is dropped, so the chat never
//!   shows the final answer twice; a turn that ends without a result
//!   flushes it before `run_turn` returns.
//! - `--permission-mode` (default `manual`) and `--allowedTools`
//!   (always `Bash(cadence *)` plus `params.allowed_tools`) are fixed at
//!   launch and replayed on resume. Denials surface as
//!   `permission_denials` on a still-successful result — recorded as an
//!   event, never a failure.
//! - `params.broker_approvals` opts the endpoint into approval
//!   brokering: `--permission-prompt-tool mcp__cadence__approve` plus a
//!   generated `--mcp-config` route every prompt the mode can't
//!   pre-decide to `agent requests`/`agent respond` (via the
//!   `cadence mcp-permission` stdio server). While a request is open
//!   the daemon stamps `note_activity` so the wait on a human never
//!   counts as turn silence. Refused with `--bypass` (moot) and `--tui`
//!   (a pane answers its own prompts).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use uuid::Uuid;

use super::link::Incoming;
use super::registry;
use super::stdio::{EnvScrub, StdioAdapter};
use super::{AdapterHooks, Identity, ProviderAdapter, ProviderEnv, TurnResult};
use crate::error::{Error, Result};
use crate::store::Agent;

/// Default inactivity window: a turn is `unknown` only after no stdout
/// event for this long — never on a wall-clock deadline. Overridable via
/// `params.turn_idle_secs` (`--turn-idle-secs`); `params.turn_max_secs`
/// (`--turn-max-secs`) adds an optional absolute cap.
const DEFAULT_TURN_IDLE: Duration = Duration::from_secs(900);
/// After SIGINT the provider is expected to emit a final `result` —
/// a bounded grace keeps a hung interrupt from parking the actor.
const INTERRUPT_GRACE: Duration = Duration::from_secs(60);
/// Request ids of Cadence's interrupt control requests.
const INTERRUPT_REQUEST_PREFIX: &str = "cadence-interrupt-";

/// Map a `result` event to a Cadence status. `interrupted_here` is true
/// when Cadence asked the CLI to interrupt this turn: then any error
/// result is the interrupt's, not a failure. A clean `success` still
/// completes — the turn finished before the interrupt landed.
fn result_status(result: &Value, interrupted_here: bool) -> &'static str {
    let is_error = result
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let subtype = result
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let aborted = result
        .get("terminal_reason")
        .and_then(Value::as_str)
        .is_some_and(|r| r.starts_with("aborted"));
    if subtype.contains("interrupt") || aborted {
        "interrupted"
    } else if is_error || subtype.starts_with("error") {
        if interrupted_here {
            "interrupted"
        } else {
            "failed"
        }
    } else if subtype == "success" || subtype.is_empty() {
        "completed"
    } else {
        // An unrecognized terminal subtype is still a definitive
        // provider answer — fail the message, never fence on it.
        "failed"
    }
}

/// Scrubbed by rule, not by name list — a list keeps missing new leak
/// variables (`CLAUDE_CODE_SUBAGENT_MODEL`, `CLAUDE_EFFORT`,
/// `CLAUDE_PID`, …). `CLAUDECODE` and every inherited `CLAUDE_*`,
/// `CODEX_*`, `CADENCE_*` name is removed except the keep-list —
/// configuration an operator sets on purpose: `CLAUDE_CONFIG_DIR`
/// (config location) and `CLAUDE_CODE_OAUTH_TOKEN` (CI auth injection).
/// `ANTHROPIC_*` auth/proxy variables are never touched. `CADENCE_*` is
/// scrubbed then the real pair (`CADENCE_ALIAS`, `CADENCE_STATE_DIR`) is
/// re-injected per agent, with the daemon's tracker and profile
/// ([`super::DAEMON_CONTEXT_ENV`]) — test overrides like
/// `CADENCE_CLAUDE_COMMAND` never reach the child.
pub(crate) fn claude_env_scrub() -> EnvScrub {
    EnvScrub::prefixes(
        &["CLAUDE_", "CLAUDECODE", "CODEX_", "CADENCE_"],
        &["CLAUDE_CONFIG_DIR", "CLAUDE_CODE_OAUTH_TOKEN"],
    )
    .and_names(super::CLOUD_SECRET_ENV)
}

/// Provider binary; `CADENCE_CLAUDE_COMMAND` overrides it (test/mock).
/// Stream-json flags are appended after this prefix so a mock sees the
/// same argv shape (`--resume`, `--permission-mode`, …) as the real CLI.
fn claude_command(env: &ProviderEnv) -> Vec<String> {
    if let Some(cmd) = env.var("CADENCE_CLAUDE_COMMAND") {
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
/// `params.model`/`params.effort` are replayed verbatim on every resume — the launch
/// line is rebuilt from the durable params, never from memory.
/// `mcp_config` is the generated broker config path when
/// `params.broker_approvals` is set — it lands beside the permission
/// flags so every resume replays the broker wiring too.
fn build_command(
    env: &ProviderEnv,
    agent: &Agent,
    session_id: &str,
    resume: bool,
    mcp_config: Option<&Path>,
) -> Vec<String> {
    let params = agent.params.clone().unwrap_or(Value::Null);
    let mut cmd = claude_command(env);
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
    // Only the configured param — `agent.model` holds the model the
    // provider reported, and replaying it would pin a cleared model.
    if let Some(model) = params.get("model").and_then(Value::as_str) {
        cmd.extend(["--model".to_string(), model.to_string()]);
    }
    if let Some(effort) = params.get("effort").and_then(Value::as_str) {
        cmd.extend(["--effort".to_string(), effort.to_string()]);
    }
    // CAD-339: the master's posture is fixed by its alias, never by
    // stored params (review round 1, C1/I4): only the Bash tool, only
    // the listed `cadence` subcommands, deny-by-default without prompts
    // (`dontAsk`), no user/project/local settings files, hooks or MCP
    // servers (`--restricted`, `--strict-mcp-config`).
    if crate::master::is_master(&agent.alias) {
        cmd.extend([
            "--restricted".to_string(),
            "--strict-mcp-config".to_string(),
            "--tools".to_string(),
            crate::master::CLAUDE_TOOLS.to_string(),
            "--permission-mode".to_string(),
            "dontAsk".to_string(),
        ]);
        for tool in crate::master::CLAUDE_ALLOWED_TOOLS {
            cmd.extend(["--allowedTools".to_string(), tool.to_string()]);
        }
        for tool in crate::master::CLAUDE_DISALLOWED_TOOLS {
            cmd.extend(["--disallowedTools".to_string(), tool.to_string()]);
        }
        return cmd;
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
    if let Some(config) = mcp_config {
        cmd.extend([
            "--mcp-config".to_string(),
            config.to_string_lossy().to_string(),
            "--strict-mcp-config".to_string(),
            "--permission-prompt-tool".to_string(),
            "mcp__cadence__approve".to_string(),
        ]);
    }
    cmd
}

/// The command the generated mcp-config points at — this same binary's
/// hidden `mcp-permission` subcommand. `CADENCE_MCP_PERMISSION_COMMAND`
/// overrides the binary path (tests point it at the built binary —
/// `current_exe` there is the test runner).
fn mcp_permission_command(env: &ProviderEnv) -> Vec<String> {
    let exe = env
        .var("CADENCE_MCP_PERMISSION_COMMAND")
        .filter(|c| !c.trim().is_empty())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "cadence".to_string());
    vec![exe, "mcp-permission".to_string()]
}

/// The full launch argv: [`build_command`], and for the master that
/// line wrapped in `cadence confine` with its policy (CAD-439) — Claude
/// Code auto-allows read-only Bash commands whatever the allowlist
/// says, so the OS decides what the master's process tree can read.
fn launch_command(
    env: &ProviderEnv,
    state_dir: &Path,
    agent: &Agent,
    session_id: &str,
    resume: bool,
    mcp_config: Option<&Path>,
) -> (Vec<String>, Option<crate::confine::Policy>) {
    let command = build_command(env, agent, session_id, resume, mcp_config);
    if !master_confined(env, agent) {
        return (command, None);
    }
    let (confine, policy) = master_confinement(env, state_dir);
    let argv = crate::master::confine_argv(&confine, &policy, &command);
    (argv, Some(policy))
}

/// The master, confined whenever this host can confine it — the stored
/// `unconfined` param (the operator's `--unconfined` on a host without
/// Landlock) counts only where it cannot (review round 2).
fn master_confined(env: &ProviderEnv, agent: &Agent) -> bool {
    crate::master::is_master(&agent.alias)
        && crate::master::is_confined(
            agent.params.as_ref(),
            crate::master::confinement_available(env).is_ok(),
        )
}

/// The confining binary and the policy a master launched by a daemon
/// with this `env` and `state_dir` gets — what `launch_command` wraps
/// the provider in (`cadence master confinement` prints it).
pub fn master_confinement(env: &ProviderEnv, state_dir: &Path) -> (String, crate::confine::Policy) {
    let command = claude_command(env);
    (
        confine_command(env),
        crate::master::confinement(&master_confine_inputs(env, state_dir, &command)),
    )
}

/// The cadence binary that runs `confine` for the master: this binary.
/// Debug builds only, the daemon's own `CADENCE_CONFINE_COMMAND` (never
/// the process env) overrides it — tests point it at the built binary,
/// `current_exe` there being the test runner.
fn confine_command(env: &ProviderEnv) -> String {
    #[cfg(debug_assertions)]
    let own = env
        .own("CADENCE_CONFINE_COMMAND")
        .filter(|c| !c.trim().is_empty());
    #[cfg(not(debug_assertions))]
    let own: Option<String> = {
        let _ = env;
        None
    };
    own.or_else(|| {
        std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    })
    .unwrap_or_else(|| "cadence".to_string())
}

/// What the master's confinement is computed from, read from this
/// daemon's env: `HOME`, `PATH` (to find the provider CLI and the
/// `cadence` the master runs), the tracker and the operator's extra
/// paths.
fn master_confine_inputs(
    env: &ProviderEnv,
    state_dir: &Path,
    command: &[String],
) -> crate::master::ConfineInputs {
    use crate::master::{split_paths, which, with_interpreter};
    let path = env.var("PATH");
    let mut programs = Vec::new();
    if let Some(p) = command.first().and_then(|c| which(c, path.as_deref())) {
        programs.extend(with_interpreter(&p, path.as_deref()));
    }
    if let Some(p) = which("cadence", path.as_deref()) {
        programs.push(p);
    }
    programs.push(PathBuf::from(confine_command(env)));
    let pm_dir = env
        .var("CADENCE_PM_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::issue::default_dir().ok());
    crate::master::ConfineInputs {
        state_dir: state_dir.to_path_buf(),
        home: env.var("HOME").filter(|h| !h.is_empty()).map(PathBuf::from),
        pm_dir,
        programs,
        // Claude's own provider dir — never `master/pi` (CAD-322, N1).
        provider_dir: crate::master::claude_config_dir(state_dir),
        home_read: crate::master::CONFINE_HOME_READ,
        extra_read: split_paths(env.var(crate::master::CONFINE_EXTRA_READ_ENV)),
        extra_write: split_paths(env.var(crate::master::CONFINE_EXTRA_WRITE_ENV)),
    }
}

/// Is this agent opted into approval brokering.
fn brokered(agent: &Agent) -> bool {
    agent
        .params
        .as_ref()
        .and_then(|p| p.get("broker_approvals"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub struct ClaudeAdapter {
    /// Swapped at `open()` — the real command line carries the session
    /// flags (`--session-id`/`--resume`, model, permissions) that only
    /// exist once the agent row is read.
    transport: RwLock<Arc<StdioAdapter>>,
    shared: Arc<Shared>,
    log_path: PathBuf,
    state_dir: PathBuf,
    /// This daemon's launch overrides — read at every `open`.
    env: ProviderEnv,
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
    /// The turn token `run_turn` is waiting on — set once the user line
    /// is written, cleared when it returns. `interrupt_turn` targets
    /// only this turn (CAD-323).
    active_turn: Mutex<Option<String>>,
    /// Last provider stdout event — the turn liveness clock. Stamped in
    /// `dispatch`, so every parsed line (assistant, user, system,
    /// stream_event, result) counts as activity.
    last_activity: Mutex<Instant>,
    /// Inactivity window before a turn is `unknown`
    /// (`params.turn_idle_secs`, default 900 s). Set at `open`.
    idle_window: Mutex<Duration>,
    /// Optional absolute turn cap (`params.turn_max_secs`, default
    /// none). Set at `open`.
    max_turn: Mutex<Option<Duration>>,
    /// The latest assistant text block, not yet emitted — flushed by
    /// the next event or by a turn ending without a result, dropped when
    /// the `result` repeats it (CAD-320).
    pending_text: Mutex<Option<String>>,
    dead: AtomicBool,
}

impl ClaudeAdapter {
    pub fn new(hooks: AdapterHooks, log_path: &Path, env: &ProviderEnv) -> Self {
        let shared = Arc::new(Shared {
            hooks,
            results: Mutex::new(VecDeque::new()),
            result_cv: Condvar::new(),
            expected_session: Mutex::new(None),
            session_mismatch: Mutex::new(None),
            generation: Mutex::new(String::new()),
            interrupt_at: Mutex::new(None),
            active_turn: Mutex::new(None),
            last_activity: Mutex::new(Instant::now()),
            idle_window: Mutex::new(DEFAULT_TURN_IDLE),
            max_turn: Mutex::new(None),
            pending_text: Mutex::new(None),
            dead: AtomicBool::new(false),
        });
        let routed = Arc::clone(&shared);
        let disconnected = Arc::clone(&shared);
        Self {
            transport: RwLock::new(StdioAdapter::new_lines(
                &claude_command(env),
                claude_env_scrub(),
                Box::new(move |incoming| routed.dispatch(incoming)),
                Box::new(move || disconnected.on_disconnect()),
            )),
            shared,
            log_path: log_path.to_path_buf(),
            env: env.clone(),
            state_dir: log_path
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")),
        }
    }

    /// The master's confinement, appended to its provider log so an
    /// operator can see why a path is unreadable (CAD-439).
    fn log_confinement(&self, policy: &crate::confine::Policy) -> Result<()> {
        if let Some(dir) = self.log_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;
        std::io::Write::write_all(
            &mut log,
            format!(
                "cadence: master confinement: {}\n",
                policy.to_args().join(" ")
            )
            .as_bytes(),
        )?;
        Ok(())
    }

    /// A transport bound to `command` but wired into this adapter's
    /// shared state — the swap target at `open()`. The master's scrub
    /// also drops forge and platform credentials (CAD-339).
    fn transport_for(&self, command: &[String], master: bool) -> Arc<StdioAdapter> {
        let routed = Arc::clone(&self.shared);
        let disconnected = Arc::clone(&self.shared);
        let scrub = if master {
            claude_env_scrub().and_names(crate::master::DENIED_ENV)
        } else {
            claude_env_scrub()
        };
        StdioAdapter::new_lines(
            command,
            scrub,
            Box::new(move |incoming| routed.dispatch(incoming)),
            Box::new(move || disconnected.on_disconnect()),
        )
    }

    /// The `--mcp-config` file a brokered agent launches with:
    /// `agents/<alias>.mcp.json` under the state dir, naming the
    /// `cadence mcp-permission` stdio server. Identity env is explicit
    /// in the config rather than inherited through the provider — the
    /// server must find its agent and socket no matter how the CLI
    /// filters the environment it hands to MCP children.
    fn write_mcp_config(&self, agent: &Agent) -> Result<PathBuf> {
        let path = self
            .state_dir
            .join("agents")
            .join(format!("{}.mcp.json", agent.alias));
        let cmd = mcp_permission_command(&self.env);
        let timeout = agent
            .params
            .as_ref()
            .and_then(|p| p.get("permission_timeout_secs"))
            .and_then(Value::as_u64)
            .unwrap_or(900);
        let config = json!({"mcpServers": {"cadence": {
        "command": cmd[0],
        "args": cmd[1..],
        "env": {
            "CADENCE_ALIAS": agent.alias,
            "CADENCE_STATE_DIR": self.state_dir.to_string_lossy(),
            "CADENCE_PERMISSION_TIMEOUT_SECS": timeout.to_string(),
        }}}});
        // Temp + rename — a relaunch never leaves a torn config behind.
        let tmp = path.with_file_name(format!(
            "{}.tmp",
            path.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&tmp, config.to_string())?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }
}

impl Shared {
    fn dispatch(&self, incoming: Incoming) {
        let Incoming::Notification { method, params } = incoming else {
            // stream-json has no server→client requests.
            return;
        };
        // Any parsed stdout event is proof of life — the turn liveness
        // clock is activity, not wall clock.
        *self.last_activity.lock().unwrap() = Instant::now();
        match method.as_str() {
            "system" if params.get("subtype").and_then(Value::as_str) == Some("init") => {
                self.on_init(&params);
            }
            // CAD-324: the CLI compacted the session's context — the
            // daemon gives the next turn a continuity pack.
            "system"
                if params.get("subtype").and_then(Value::as_str) == Some("compact_boundary") =>
            {
                let meta = params.get("compact_metadata").unwrap_or(&Value::Null);
                self.emit(
                    "cadence/session_compacted",
                    &json!({"trigger": meta.get("trigger"), "pre_tokens": meta.get("pre_tokens")}),
                );
            }
            "assistant" => self.on_assistant(&params),
            "user" => self.on_tool_results(&params),
            "control_response" => self.on_control_response(&params),
            "result" => {
                // Before the result is queued: the turn's entries land
                // ahead of its `turn_result`.
                self.flush_text(params.get("result").and_then(Value::as_str));
                self.results.lock().unwrap().push_back(params.clone());
                self.result_cv.notify_all();
                self.emit_result_meta(&params);
            }
            _ => {}
        }
    }

    /// One compact lifecycle event per tool use — the name and a
    /// one-line redacted summary ([`crate::store::tool_summary`], CAD-319),
    /// never the raw arguments — so `cadence events --follow` and a
    /// threaded agent's chat show progress on a long turn without
    /// proxying the transcript. Text blocks are held one at a time for
    /// the chat ([`Self::flush_text`], CAD-320).
    fn on_assistant(&self, event: &Value) {
        let Some(content) = event
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            return;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                if !text.trim().is_empty() {
                    self.flush_text(None);
                    *self.pending_text.lock().unwrap() = Some(text.to_string());
                }
            }
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                self.flush_text(None);
                if let Some(name) = block.get("name").and_then(Value::as_str) {
                    let input = block.get("input").unwrap_or(&Value::Null);
                    self.emit(
                        "cadence/tool_use",
                        &json!({
                            "tool": name,
                            "summary": crate::store::tool_summary(name, input),
                            "tool_use_id": block.get("id"),
                        }),
                    );
                }
            }
        }
    }

    /// Emit the held assistant text block, unless it is `final_text` —
    /// the `result` carries that one as the turn result.
    fn flush_text(&self, final_text: Option<&str>) {
        let Some(text) = self.pending_text.lock().unwrap().take() else {
            return;
        };
        if final_text.is_some_and(|f| f.trim() == text.trim()) {
            return;
        }
        self.emit("cadence/assistant_text", &json!({ "text": text }));
    }

    /// One `tool_result` event per tool result block in a `user` event —
    /// a redacted one-line summary ([`crate::store::tool_result_summary`])
    /// and `is_error`, never the output itself (CAD-320).
    fn on_tool_results(&self, event: &Value) {
        let Some(content) = event
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            return;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            self.flush_text(None);
            let output = block.get("content").unwrap_or(&Value::Null);
            self.emit(
                "cadence/tool_result",
                &json!({
                    "summary": crate::store::tool_result_summary(output),
                    "is_error": block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    "tool_use_id": block.get("tool_use_id"),
                }),
            );
        }
    }

    /// The CLI's answer to our interrupt control request — recorded, so
    /// a refused interrupt is visible on the agent (CAD-323). Only our
    /// own request ids are ours to report.
    fn on_control_response(&self, event: &Value) {
        let response = event.get("response").unwrap_or(&Value::Null);
        let request_id = response
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !request_id.starts_with(INTERRUPT_REQUEST_PREFIX) {
            return;
        }
        self.emit(
            "cadence/interrupt_ack",
            &json!({
                "request_id": request_id,
                "subtype": response.get("subtype"),
                "error": response.get("error"),
            }),
        );
    }

    /// The session id in `system/init` is authoritative identity proof:
    /// anything but the id this process was opened with means another
    /// Claude owns the expected session — fail closed into `attention`.
    fn on_init(&self, event: &Value) {
        // The model the CLI actually runs — inherited settings included —
        // recorded as the agent's reported model.
        if let Some(model) = event.get("model").and_then(Value::as_str) {
            self.emit("cadence/claude_init", &json!({"model": model}));
        }
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
            // The event lane is unscoped: a denial keeps the routing
            // fields and the same redacted one-line summary a tool_use
            // carries — never `tool_input` verbatim. The declined input
            // is the most dangerous subset of the transcript (the
            // operator declined because it looked dangerous); the full
            // wire object stays in the provider transcript (CAD-542).
            let denials: Vec<Value> = denials
                .iter()
                .map(|denial| {
                    let name = denial
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    json!({
                        "tool_name": denial.get("tool_name"),
                        "tool_use_id": denial.get("tool_use_id"),
                        "summary": crate::store::tool_summary(
                            name,
                            denial.get("tool_input").unwrap_or(&Value::Null),
                        ),
                    })
                })
                .collect();
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
    /// A brokered permission request is open — the provider is silent
    /// while a human decides, and that wait is activity, not idleness.
    fn note_activity(&self) {
        *self.shared.last_activity.lock().unwrap() = Instant::now();
    }

    /// The transcript clock — every parsed provider notification bumps
    /// it in `Shared::dispatch`, so the daemon's stall watch reads the
    /// same liveness signal the turn's own idle check uses.
    fn activity_at(&self) -> Option<Instant> {
        Some(*self.shared.last_activity.lock().unwrap())
    }

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
        // Brokered approvals get a generated mcp-config beside the
        // provider log — written per open so resume replays it like
        // every other launch param, and carrying the identity env the
        // `mcp-permission` server needs explicitly (independent of the
        // provider's own env propagation).
        let master = crate::master::is_master(&agent.alias);
        // The master never brokers prompts: it runs `dontAsk`.
        let mcp_config = if brokered(agent) && !master {
            Some(self.write_mcp_config(agent)?)
        } else {
            None
        };
        let (command, confinement) = launch_command(
            &self.env,
            &self.state_dir,
            agent,
            &session_id,
            resume,
            mcp_config.as_deref(),
        );
        if let Some(policy) = &confinement {
            self.log_confinement(policy)?;
        }
        *self.shared.expected_session.lock().unwrap() = Some(session_id.clone());
        *self.shared.session_mismatch.lock().unwrap() = None;
        let generation = Uuid::new_v4().simple().to_string()[..12].to_string();
        *self.shared.generation.lock().unwrap() = generation.clone();
        self.shared.dead.store(false, Ordering::SeqCst);
        let mut env = vec![
            ("CADENCE_ALIAS".to_string(), agent.alias.clone()),
            (
                "CADENCE_STATE_DIR".to_string(),
                self.state_dir.to_string_lossy().to_string(),
            ),
        ];
        env.extend(super::daemon_context_env(&self.env));
        if master {
            // Its cwd is not the tracker: name the tracker explicitly
            // when the daemon's context does not already.
            let pm = self
                .env
                .var("CADENCE_PM_DIR")
                .filter(|v| !v.is_empty())
                .is_none()
                .then(crate::issue::default_dir)
                .and_then(Result::ok);
            env.extend(crate::master::env_overrides(
                "claude",
                &self.state_dir,
                pm.as_deref(),
                master_confined(&self.env, agent),
            ));
        }
        let params = agent.params.clone().unwrap_or(Value::Null);
        let idle_secs = params
            .get("turn_idle_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TURN_IDLE.as_secs());
        *self.shared.idle_window.lock().unwrap() = Duration::from_secs(idle_secs.max(1));
        *self.shared.max_turn.lock().unwrap() = params
            .get("turn_max_secs")
            .and_then(Value::as_u64)
            .map(|s| Duration::from_secs(s.max(1)));
        *self.shared.last_activity.lock().unwrap() = Instant::now();
        let transport = self.transport_for(&command, master);
        let pid = transport.launch(&agent.cwd, &self.log_path, &env)?;
        *self.transport.write().unwrap() = transport;
        Ok(Identity {
            thread_id: session_id.clone(),
            session_id,
            model: agent.model.clone(),
            effort: None,
            pid,
            endpoint: None,
            generation: Some(generation),
            attach: None,
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
        let turn_id = registry::CLAUDE_MANAGED_TURN_TOKENS.mint(&generation);
        // An interrupt aimed at an earlier turn (or at an idle process)
        // never shortens this one's grace.
        *self.shared.interrupt_at.lock().unwrap() = None;
        // A queued result here is stale — it belongs to a turn the
        // daemon already fenced. Discard it rather than misattribute.
        {
            let mut queue = self.shared.results.lock().unwrap();
            while let Some(stale) = queue.pop_front() {
                // The stale wire object holds the fenced turn's result
                // text and any permission denials — the lane keeps the
                // discard's shape only (CAD-542, same class as
                // permission_denied: no raw provider objects).
                self.shared.emit(
                    "cadence/stale_result",
                    &json!({
                        "subtype": stale.get("subtype"),
                        "is_error": stale.get("is_error"),
                        "stop_reason": stale.get("stop_reason"),
                        "session_id": stale.get("session_id"),
                    }),
                );
            }
        }
        self.transport.read().unwrap().send(json!({
            "type": "user",
            "message": {"role": "user", "content": prompt},
        }))?;
        // Only now is there a turn for `interrupt_turn` to stop — the
        // message is marked running (and so interruptible) just below.
        *self.shared.active_turn.lock().unwrap() = Some(turn_id.clone());
        on_started(&turn_id);
        // The liveness clock is provider activity, not wall clock: a
        // turn that keeps emitting events is alive no matter how long it
        // runs. `turn_idle_secs` bounds silence; `turn_max_secs`, when
        // set, is an absolute cap even on a chatty turn.
        let start = Instant::now();
        *self.shared.last_activity.lock().unwrap() = start;
        let idle_window = *self.shared.idle_window.lock().unwrap();
        let max_turn = *self.shared.max_turn.lock().unwrap();
        let waited = (|| -> Result<Value> {
            let mut queue = self.shared.results.lock().unwrap();
            loop {
                if let Some(mismatch) = self.shared.session_mismatch.lock().unwrap().clone() {
                    return Err(Error::provider(mismatch));
                }
                if let Some(result) = queue.pop_front() {
                    return Ok(result);
                }
                if self.shared.dead.load(Ordering::SeqCst) {
                    return Err(Error::unknown(
                        "Claude process exited before a result; outcome is unknown",
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
                let interrupt_deadline = self
                    .shared
                    .interrupt_at
                    .lock()
                    .unwrap()
                    .map(|at| at + INTERRUPT_GRACE);
                if let Some(at) = interrupt_deadline {
                    if now >= at {
                        return Err(Error::unknown(
                            "No result after interrupt; provider outcome is unknown",
                        ));
                    }
                }
                let mut remaining = idle_left;
                if let Some(d) = max_left {
                    remaining = remaining.min(d);
                }
                if let Some(at) = interrupt_deadline {
                    remaining = remaining.min(at.saturating_duration_since(now));
                }
                let (guard, _) = self
                    .shared
                    .result_cv
                    .wait_timeout(queue, remaining.min(Duration::from_millis(250)))
                    .unwrap();
                queue = guard;
            }
        })();
        // No result is coming (death, idle fence, cap, interrupt grace,
        // session mismatch): the held text block is the agent's, and it
        // must land now — linked to the running message, ahead of the
        // turn result the daemon records next (CAD-320).
        *self.shared.active_turn.lock().unwrap() = None;
        let result = waited.inspect_err(|_| self.shared.flush_text(None))?;
        let interrupted_here = self.shared.interrupt_at.lock().unwrap().take().is_some();
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
        let status = result_status(&result, interrupted_here);
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
            "managed claude endpoints broker no requests — widen \
             permissions by relaunching or rejoining with \
             `--permission-mode <mode>` or `--allow \"<pattern>\"` \
             (or `--bypass`); denials are recorded as permission_denied \
             events on the agent",
        ))
    }

    /// The CLI's own interrupt control request — Claude ends the active
    /// turn and emits its final `result`. The waiter bounds the hang to
    /// `INTERRUPT_GRACE` before failing closed. The write never blocks
    /// ([`StdioAdapter::try_send`]); only when stdin is gone, full or
    /// busy does it fall back to SIGINT on the provider's own process
    /// group.
    fn interrupt(&self) {
        *self.shared.interrupt_at.lock().unwrap() = Some(Instant::now());
        let transport = self.transport.read().unwrap().clone();
        let request = json!({
            "type": "control_request",
            "request_id": format!("{INTERRUPT_REQUEST_PREFIX}{}", Uuid::new_v4().simple()),
            "request": {"subtype": "interrupt"},
        });
        // Never blocks: a full stdin pipe or a writer mid-frame must not
        // hang an interrupt, a stop or a shutdown — SIGINT instead.
        if transport.try_send(request).is_err() {
            transport.interrupt();
        }
    }

    /// CAD-323: interrupt `turn_id` only while it is the turn in flight.
    fn interrupt_turn(
        &self,
        turn_id: &str,
        _settle: &dyn Fn() -> Result<bool>,
    ) -> Result<super::InterruptOutcome> {
        // Held across the send: `run_turn` clears the active turn under
        // this lock, so an interrupt can never land on the next turn.
        let active = self.shared.active_turn.lock().unwrap();
        if active.as_deref() != Some(turn_id) {
            return Ok(super::InterruptOutcome::NotRunning);
        }
        self.interrupt();
        Ok(super::InterruptOutcome::Delivered)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(alias: &str, params: Value) -> Agent {
        Agent {
            alias: alias.into(),
            provider: "claude".into(),
            endpoint_kind: "managed".into(),
            role: "worker".into(),
            team_role: None,
            cwd: "/tmp".into(),
            sandbox: "read-only".into(),
            instructions: None,
            thread_id: None,
            session_id: None,
            model: None,
            effort: None,
            pid: None,
            pid_start: None,
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

    fn flag_values(cmd: &[String], flag: &str) -> Vec<String> {
        cmd.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].clone())
            .collect()
    }

    /// CAD-439: the master's provider launches under `cadence confine`
    /// with its policy — the CI stand-in for the real-claude probes
    /// (`scripts/master-read-probe.sh`). The wrapped line is exactly the
    /// CAD-339 one; any other agent launches unwrapped.
    #[test]
    fn master_launch_is_confined() {
        let env = ProviderEnv::default();
        env.set("CADENCE_CONFINE_COMMAND", "/opt/cadence/bin/cadence");
        env.set("HOME", "/h");
        env.set("CADENCE_PM_DIR", "/pm");
        let state = Path::new("/s");
        let master = agent("master", json!({"permission_mode": "bypassPermissions"}));
        let (argv, policy) = launch_command(&env, state, &master, "s", false, None);
        let policy = policy.expect("the master is confined");
        let inner = build_command(&env, &master, "s", false, None);
        let mut want = vec![
            "/opt/cadence/bin/cadence".to_string(),
            "confine".to_string(),
        ];
        want.extend(policy.to_args());
        want.push("--".to_string());
        want.extend(inner.iter().cloned());
        assert_eq!(argv, want);
        assert_eq!(flag_values(&inner, "--permission-mode"), ["dontAsk"]);
        let reads = flag_values(&argv, "--read");
        let writes = flag_values(&argv, "--write");
        for p in ["/usr", "/etc", "/proc/self", "/h/.local/share/claude"] {
            assert!(reads.iter().any(|r| r == p), "{p}: {argv:?}");
        }
        for p in ["/s/master/cwd", "/s/master/tmp", "/s/master/claude", "/pm"] {
            assert!(writes.iter().any(|w| w == p), "{p}: {argv:?}");
        }
        for p in [
            "/",
            "/h",
            "/h/.claude",
            "/h/.claude.json",
            "/s",
            "/tmp",
            "/proc",
            "/dev",
            "/dev/pts",
        ] {
            assert!(
                !reads.iter().chain(&writes).any(|r| r == p),
                "{p}: {argv:?}"
            );
        }
        // Resume keeps the wrapper.
        let (argv, _) = launch_command(&env, state, &master, "s", true, None);
        assert_eq!(argv[..2], ["/opt/cadence/bin/cadence", "confine"]);
        assert!(argv.iter().any(|a| a == "--resume"));
        // A stored `unconfined` param does not unconfine the master where
        // the host can confine it (review round 2)…
        let loose = agent("master", json!({"unconfined": true}));
        if crate::confine::available().is_ok() {
            let (argv, policy) = launch_command(&env, state, &loose, "s", false, None);
            assert!(policy.is_some(), "{argv:?}");
            assert_eq!(argv[..2], ["/opt/cadence/bin/cadence", "confine"]);
        }
        // …only where it cannot (the operator's `--unconfined`): the
        // CAD-339 line, unwrapped.
        env.set(crate::master::TEST_NO_LANDLOCK, "1");
        let (argv, policy) = launch_command(&env, state, &loose, "s", false, None);
        assert!(policy.is_none());
        assert_eq!(argv, build_command(&env, &loose, "s", false, None));
        assert_eq!(flag_values(&argv, "--permission-mode"), ["dontAsk"]);
        // Without the param, a host without Landlock never launches it.
        let (argv, policy) = launch_command(&env, state, &master, "s", false, None);
        assert!(policy.is_some() && argv[1] == "confine");
        env.remove(crate::master::TEST_NO_LANDLOCK);
        // Any other agent: no wrapper, no policy.
        let dev = agent("dev-1", json!({}));
        let (argv, policy) = launch_command(&env, state, &dev, "s", false, None);
        assert!(policy.is_none());
        assert_eq!(argv, build_command(&env, &dev, "s", false, None));
        assert!(!argv.iter().any(|a| a == "confine"));
    }

    /// CAD-339: the master's launch line ignores stored permission
    /// params — `cadence` commands only, edits, gh and push disallowed —
    /// while any other agent keeps its params.
    #[test]
    fn master_tool_posture_is_fixed_by_alias() {
        let env = ProviderEnv::default();
        let loose = json!({"permission_mode": "bypassPermissions",
                           "allowed_tools": ["Bash(gh *)", "Edit"]});
        let cmd = build_command(&env, &agent("master", loose.clone()), "s", false, None);
        assert_eq!(flag_values(&cmd, "--permission-mode"), ["dontAsk"]);
        assert_eq!(flag_values(&cmd, "--tools"), ["Bash"]);
        assert!(cmd.iter().any(|a| a == "--restricted"), "{cmd:?}");
        assert!(cmd.iter().any(|a| a == "--strict-mcp-config"), "{cmd:?}");
        let allowed = flag_values(&cmd, "--allowedTools");
        assert_eq!(allowed, crate::master::CLAUDE_ALLOWED_TOOLS);
        // Review round 1, C1: never a bare `cadence *` — build-slot run
        // execs arbitrary argv — and nothing that execs or writes agents.
        for a in &allowed {
            assert!(a.starts_with("Bash(cadence "), "{a}");
            for bad in [
                "Bash(cadence *)",
                "build-slot",
                "agent set",
                "cadence send",
                "cadence dispatch",
            ] {
                assert!(!a.contains(bad), "{a}");
            }
        }
        let denied = flag_values(&cmd, "--disallowedTools");
        for tool in ["Edit", "Write", "Bash(gh *)", "Bash(git push *)"] {
            assert!(denied.iter().any(|d| d == tool), "{tool}: {cmd:?}");
        }
        let cmd = build_command(&env, &agent("dev-1", loose), "s", false, None);
        assert_eq!(
            flag_values(&cmd, "--permission-mode"),
            ["bypassPermissions"]
        );
        assert_eq!(flag_values(&cmd, "--allowedTools").len(), 3);
        assert!(flag_values(&cmd, "--disallowedTools").is_empty());
    }

    #[test]
    fn master_scrub_drops_forge_and_platform_credentials() {
        let scrub = claude_env_scrub().and_names(crate::master::DENIED_ENV);
        for name in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "SSH_AUTH_SOCK",
            "CLOUDFLARE_API_TOKEN",
        ] {
            assert!(scrub.removes_name(name), "{name}");
            assert!(!claude_env_scrub().removes_name(name), "{name}");
        }
        assert!(!scrub.removes_name("ANTHROPIC_API_KEY"));
        assert!(!scrub.removes_name("PATH"));
    }

    /// CAD-323: an aborted result is `interrupted`; an error result is
    /// the interrupt's only when Cadence asked for one; a clean success
    /// completes even then (the turn beat the interrupt).
    #[test]
    fn result_status_maps_interrupts() {
        let aborted = json!({"subtype": "error_during_execution", "is_error": true,
                             "terminal_reason": "aborted_tools"});
        assert_eq!(result_status(&aborted, false), "interrupted");
        let streaming = json!({"subtype": "success", "is_error": false,
                               "terminal_reason": "aborted_streaming"});
        assert_eq!(result_status(&streaming, false), "interrupted");
        assert_eq!(
            result_status(&json!({"subtype": "interrupted"}), false),
            "interrupted"
        );
        let error = json!({"subtype": "error_during_execution", "is_error": true});
        assert_eq!(result_status(&error, false), "failed");
        assert_eq!(result_status(&error, true), "interrupted");
        let success = json!({"subtype": "success", "is_error": false});
        assert_eq!(result_status(&success, true), "completed");
        assert_eq!(result_status(&success, false), "completed");
        assert_eq!(result_status(&json!({"subtype": "odd"}), false), "failed");
    }
}
