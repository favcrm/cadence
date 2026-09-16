//! `cadence` — CLI client + daemon entrypoint.
//!
//! Commands that are not implemented in this milestone fail loudly rather
//! than pretending to work.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use cadence_agent::client;
use cadence_agent::error::{Error, Result};

#[derive(Parser)]
#[command(
    name = "cadence",
    about = "Local controller for coding agents",
    version
)]
struct Cli {
    /// Runtime state directory (socket, database, logs).
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check environment, storage and provider CLIs.
    Doctor,
    /// Manage the persistent controller.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Manage registered agents.
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Send durable messages to an agent.
    Message {
        #[command(subcommand)]
        action: MessageAction,
    },
    /// Read the durable event log for an agent.
    Events {
        /// Agent alias.
        alias: String,
        /// Return events after this cursor.
        #[arg(long, default_value_t = 0)]
        after: i64,
        /// Seconds to wait for new events per request (0-30).
        #[arg(long, default_value_t = 0)]
        wait: u64,
        /// Keep streaming new events until interrupted.
        #[arg(long)]
        follow: bool,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Run the daemon in the foreground.
    Run,
    /// Start the daemon detached and print its state.
    Start,
    /// Report daemon health.
    Status,
    /// Ask the daemon to shut down gracefully.
    Stop,
}

#[derive(Subcommand)]
enum AgentAction {
    /// Register an agent and start its actor.
    Register {
        alias: String,
        /// Provider driver: codex (managed) or fake (test double).
        #[arg(long)]
        provider: String,
        /// Endpoint kind: managed (stdio), managed-ws (official-TUI
        /// attachable WebSocket app-server), pty (official TUI in an
        /// owned tmux session; devin only) or fake (test double).
        #[arg(long, default_value = "managed")]
        endpoint: String,
        /// Working directory for the provider session.
        #[arg(long)]
        cwd: PathBuf,
        /// pm or worker.
        #[arg(long, default_value = "worker")]
        role: String,
        /// read-only or workspace-write.
        #[arg(long, default_value = "read-only")]
        sandbox: String,
        /// File with reusable provider instructions.
        #[arg(long)]
        instructions_file: Option<PathBuf>,
        /// Endpoint option as key=value (pty: session=<native-id> to
        /// resume an existing Devin session). Repeatable.
        #[arg(long = "param")]
        params: Vec<String>,
    },
    /// List registered agents.
    List,
    /// Show one agent, its messages and event cursor.
    Show { alias: String },
    /// List pending provider requests (approvals, input).
    Requests { alias: String },
    /// Answer a pending provider request.
    Respond {
        alias: String,
        /// Request handle from `agent requests`.
        #[arg(long)]
        request: String,
        /// accept or decline for approval requests.
        #[arg(long)]
        decision: Option<String>,
        /// JSON answers file for input requests.
        #[arg(long)]
        answers_file: Option<PathBuf>,
    },
    /// Stop the agent's actor (queued messages are retained).
    Stop { alias: String },
    /// Resume a stopped agent on its saved native thread.
    Resume { alias: String },
    /// Show or run the official attach command for an attachable
    /// endpoint (managed-ws: `codex resume --remote`; pty: tmux attach).
    Attach {
        alias: String,
        /// Execute the attach in this terminal instead of printing it.
        #[arg(long)]
        run: bool,
    },
    /// Claim a gated endpoint is ready for one submission (pty only).
    /// Asserts the operator inspected the terminal: idle, empty input,
    /// no permission prompt. Consumed by a single send, expires quickly.
    Ready { alias: String },
    /// Print the current terminal contents of a pty endpoint.
    Capture { alias: String },
}

#[derive(Subcommand)]
enum MessageAction {
    /// Enqueue a message; returns once it is durable.
    Send {
        alias: String,
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        /// Idempotency key; retries with the same id+content dedupe.
        #[arg(long)]
        message: Option<String>,
        /// Route the result to another agent when the turn finishes.
        #[arg(long)]
        reply_to: Option<String>,
    },
    /// Send and wait for the turn's terminal state.
    Ask {
        alias: String,
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        message: Option<String>,
        /// Seconds to wait (max 600).
        #[arg(long, default_value_t = 120)]
        wait: u64,
    },
    /// Record an explicit acknowledgement for a submitted PTY message.
    /// The token is the `turn_id` shown by `agent show`.
    Ack {
        /// Message id.
        message: String,
        /// Submission token (pty-<generation>-<uuid>).
        #[arg(long)]
        token: String,
        /// Optional acknowledgement note.
        #[arg(long)]
        text: Option<String>,
    },
    /// Report the result of a submitted PTY message; completes it and
    /// routes to `reply_to` when set.
    Result {
        /// Message id.
        message: String,
        /// Submission token (pty-<generation>-<uuid>).
        #[arg(long)]
        token: String,
        /// Result text reported for the message.
        #[arg(long)]
        text: String,
    },
}

fn read_body(text: Option<String>, file: Option<PathBuf>) -> Result<String> {
    if let Some(text) = text {
        return Ok(text);
    }
    if let Some(file) = file {
        let mut body = String::new();
        std::fs::File::open(&file)?.read_to_string(&mut body)?;
        return Ok(body);
    }
    if atty_stdin() {
        return Err(Error::rejected("Provide --text or --file"));
    }
    let mut body = String::new();
    std::io::stdin().read_to_string(&mut body)?;
    Ok(body)
}

fn atty_stdin() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

/// Spawn `daemon run` detached; it survives the terminal via setsid.
fn daemon_start(state_dir: &Path) -> Result<Value> {
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("daemon.log"))?;
    let mut command = Command::new(exe);
    command
        .args(["--state-dir"])
        .arg(state_dir)
        .args(["daemon", "run"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    // Wait until the socket answers or the child exits.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client::rpc(state_dir, "health", json!({})) {
            Ok(health) => {
                // If our child already exited, the socket belongs to a
                // pre-existing daemon — report that honestly.
                if child.try_wait().ok().flatten().is_some() {
                    return Ok(json!({
                        "state": "already_running",
                        "socket": client::socket_path(state_dir),
                        "health": health,
                    }));
                }
                return Ok(json!({
                    "state": "started",
                    "pid": child.id(),
                    "socket": client::socket_path(state_dir),
                    "health": health,
                }));
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e),
        }
    }
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

fn run() -> Result<i32> {
    let cli = Cli::parse();
    let state_dir = match cli.state_dir {
        Some(dir) => dir,
        None => client::state_dir()?,
    };
    match cli.command {
        Commands::Doctor => {
            let report = cadence_agent::doctor::run(&state_dir)?;
            print_json(&report);
            let ok = report
                .pointer("/checks/storage/ok")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(if ok { 0 } else { 1 })
        }
        Commands::Daemon { action } => match action {
            DaemonAction::Run => {
                std::fs::create_dir_all(&state_dir)?;
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
                }
                cadence_agent::daemon::serve(&state_dir)?;
                Ok(0)
            }
            DaemonAction::Start => {
                std::fs::create_dir_all(&state_dir)?;
                let result = daemon_start(&state_dir)?;
                print_json(&result);
                Ok(0)
            }
            DaemonAction::Status => {
                let health = client::rpc(&state_dir, "health", json!({}))?;
                print_json(&health);
                Ok(0)
            }
            DaemonAction::Stop => {
                let result = client::rpc(&state_dir, "shutdown", json!({}))?;
                print_json(&result);
                Ok(0)
            }
        },
        Commands::Agent { action } => {
            let result = match action {
                AgentAction::Register {
                    alias,
                    provider,
                    endpoint,
                    cwd,
                    role,
                    sandbox,
                    instructions_file,
                    params,
                } => {
                    let instructions = match instructions_file {
                        Some(path) => Some(std::fs::read_to_string(path)?),
                        None => None,
                    };
                    let mut obj = serde_json::Map::new();
                    for kv in &params {
                        let (k, v) = kv
                            .split_once('=')
                            .ok_or_else(|| Error::rejected("--param entries must be key=value"))?;
                        if k.is_empty() {
                            return Err(Error::rejected("--param key must not be empty"));
                        }
                        obj.insert(k.to_string(), Value::String(v.to_string()));
                    }
                    let params_json = (!obj.is_empty()).then(|| Value::Object(obj).to_string());
                    client::rpc(
                        &state_dir,
                        "agent_register",
                        json!({
                            "alias": alias, "provider": provider,
                            "endpoint_kind": endpoint, "cwd": cwd,
                            "role": role, "sandbox": sandbox,
                            "instructions": instructions,
                            "params": params_json,
                        }),
                    )?
                }
                AgentAction::List => client::rpc(&state_dir, "agent_list", json!({}))?,
                AgentAction::Show { alias } => {
                    client::rpc(&state_dir, "agent_show", json!({"alias": alias}))?
                }
                AgentAction::Requests { alias } => {
                    client::rpc(&state_dir, "agent_requests", json!({"alias": alias}))?
                }
                AgentAction::Respond {
                    alias,
                    request,
                    decision,
                    answers_file,
                } => {
                    let answers = match answers_file {
                        Some(path) => Some(serde_json::from_str::<Value>(
                            &std::fs::read_to_string(path)?,
                        )?),
                        None => None,
                    };
                    client::rpc(
                        &state_dir,
                        "agent_respond",
                        json!({"alias": alias, "request": request,
                               "decision": decision, "answers": answers}),
                    )?
                }
                AgentAction::Stop { alias } => {
                    client::rpc(&state_dir, "agent_stop", json!({"alias": alias}))?
                }
                AgentAction::Resume { alias } => {
                    client::rpc(&state_dir, "agent_resume", json!({"alias": alias}))?
                }
                AgentAction::Attach { alias, run } => {
                    let show = client::rpc(&state_dir, "agent_show", json!({"alias": alias}))?;
                    let agent = &show["agent"];
                    let kind = agent["endpoint_kind"].as_str().unwrap_or_default();
                    if !matches!(kind, "managed-ws" | "pty") {
                        return Err(Error::rejected(format!(
                            "Agent '{alias}' uses endpoint kind '{kind}'; attach requires \
                             'managed-ws' or 'pty'"
                        )));
                    }
                    let state = agent["state"].as_str().unwrap_or_default();
                    if matches!(state, "stopped" | "offline") {
                        return Err(Error::rejected(format!(
                            "Agent '{alias}' is {state} — resume it before attaching"
                        )));
                    }
                    let endpoint = agent["endpoint"].as_str().ok_or_else(|| {
                        Error::rejected(
                            "No live endpoint — the agent's provider endpoint is not \
                             running (start or resume the agent first)",
                        )
                    })?;
                    let thread = agent["thread_id"]
                        .as_str()
                        .ok_or_else(|| Error::rejected("Agent has no native thread yet"))?;
                    if kind == "pty" {
                        // tmux://<socket>/<session> — attach is a view of
                        // the owned pane, not a takeover of anything else.
                        let (socket, session) = endpoint
                            .strip_prefix("tmux://")
                            .and_then(|rest| rest.split_once('/'))
                            .ok_or_else(|| Error::internal("malformed tmux endpoint"))?;
                        if run {
                            let status = Command::new("tmux")
                                .args(["-L", socket, "attach-session", "-t", session])
                                .status()?;
                            return Ok(status.code().unwrap_or(1));
                        }
                        print_json(&json!({
                            "alias": alias,
                            "endpoint": endpoint,
                            "thread_id": thread,
                            "command": format!(
                                "tmux -L {socket} attach-session -t {session}"
                            ),
                            "note": "Attach shows the live pane; terminal echo is not \
                                     agent receipt — message state remains authoritative.",
                        }));
                        return Ok(0);
                    }
                    if run {
                        let status = Command::new("codex")
                            .args(["resume", "--remote", endpoint, thread])
                            .status()?;
                        return Ok(status.code().unwrap_or(1));
                    }
                    json!({
                        "alias": alias,
                        "endpoint": endpoint,
                        "thread_id": thread,
                        "command": format!("codex resume --remote {endpoint} {thread}"),
                        "note": "Attach shows the native thread; terminal echo is not \
                                 agent receipt — message state remains authoritative.",
                    })
                }
                AgentAction::Ready { alias } => {
                    client::rpc(&state_dir, "agent_ready", json!({"alias": alias}))?
                }
                AgentAction::Capture { alias } => {
                    let out = client::rpc(&state_dir, "agent_capture", json!({"alias": alias}))?;
                    if let Some(text) = out["capture"].as_str() {
                        println!("{text}");
                        return Ok(0);
                    }
                    out
                }
            };
            print_json(&result);
            Ok(0)
        }
        Commands::Message { action } => {
            let (result, pending) = match action {
                MessageAction::Send {
                    alias,
                    text,
                    file,
                    message,
                    reply_to,
                } => {
                    let body = read_body(text, file)?;
                    (
                        client::rpc(
                            &state_dir,
                            "agent_send",
                            json!({"alias": alias, "text": body,
                                   "message": message, "reply_to": reply_to}),
                        )?,
                        false,
                    )
                }
                MessageAction::Ack {
                    message,
                    token,
                    text,
                } => (
                    client::rpc(
                        &state_dir,
                        "message_report",
                        json!({"message": message, "token": token,
                               "kind": "ack", "text": text}),
                    )?,
                    false,
                ),
                MessageAction::Result {
                    message,
                    token,
                    text,
                } => (
                    client::rpc(
                        &state_dir,
                        "message_report",
                        json!({"message": message, "token": token,
                               "kind": "result", "text": text}),
                    )?,
                    false,
                ),
                MessageAction::Ask {
                    alias,
                    text,
                    file,
                    message,
                    wait,
                } => {
                    let body = read_body(text, file)?;
                    let result = client::rpc(
                        &state_dir,
                        "agent_ask",
                        json!({"alias": alias, "text": body,
                               "message": message, "wait": wait}),
                    )?;
                    let state = result
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let pending = matches!(state, "queued" | "submitting" | "running");
                    (result, pending)
                }
            };
            print_json(&result);
            Ok(if pending { 2 } else { 0 })
        }
        Commands::Events {
            alias,
            after,
            wait,
            follow,
        } => {
            let mut cursor = after;
            loop {
                let page = client::rpc(
                    &state_dir,
                    "agent_events",
                    json!({"alias": alias, "after": cursor,
                           "wait": if follow { 25 } else { wait }}),
                )?;
                let empty = page
                    .get("events")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty);
                if !empty || !follow {
                    print_json(&page);
                }
                cursor = page.get("cursor").and_then(Value::as_i64).unwrap_or(cursor);
                if !follow {
                    return Ok(0);
                }
            }
        }
    }
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "error": error.to_string(), "kind": error.kind(),
                }))
                .unwrap_or_default()
            );
            1
        }
    };
    std::process::exit(code);
}
