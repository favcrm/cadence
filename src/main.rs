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
use uuid::Uuid;

use cadence_agent::client;
use cadence_agent::error::{Error, Result};
use cadence_agent::proto;

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
    /// Launch a Devin official terminal as a managed agent (pty endpoint).
    /// `-r <session-slug>` resumes an existing Devin session, mirroring
    /// `devin -r`; without it a fresh session is launched and becomes
    /// addressable by its discovered slug. Once the endpoint is open this
    /// terminal attaches to the owned pane by default (`--detach` opts
    /// out; a non-TTY or nested-tmux launch prints the command instead).
    Devin {
        /// Resume an existing Devin session by its native slug.
        #[arg(short = 'r', long)]
        resume: Option<String>,
        /// Do not attach this terminal to the owned pane once open.
        #[arg(long)]
        detach: bool,
        /// Working directory for the session [default: current directory].
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Routing alias [default: the resumed slug, else devin-<random>].
        #[arg(long)]
        alias: Option<String>,
        /// pm or worker.
        #[arg(long, default_value = "worker")]
        role: String,
        /// File with reusable provider instructions.
        #[arg(long)]
        instructions_file: Option<PathBuf>,
        /// Run the session in an isolated checkout:
        /// `git worktree add <repo>/.cadence/wt/<name> -b cadence/<name>`
        /// becomes the agent's cwd.
        #[arg(long)]
        worktree: Option<String>,
    },
    /// Launch a Codex agent on a managed-ws endpoint, attachable by the
    /// official Codex TUI via `codex resume --remote`. This terminal
    /// runs that attach once the endpoint is up by default (`--detach`
    /// opts out; a non-TTY or nested-tmux launch prints the command).
    Codex {
        /// Do not attach this terminal once the endpoint is up.
        #[arg(long)]
        detach: bool,
        /// Working directory for the session [default: current directory].
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Routing alias [default: codex-<random>].
        #[arg(long)]
        alias: Option<String>,
        /// pm or worker.
        #[arg(long, default_value = "worker")]
        role: String,
        /// File with reusable provider instructions.
        #[arg(long)]
        instructions_file: Option<PathBuf>,
    },
    /// Join a new worker agent to a group. `<group>` is the PM agent —
    /// its alias or provider-native id — and `<provider>` is devin,
    /// codex or fake. The worker's results route back to the PM by
    /// default (its params gain `"upstream"`). This terminal attaches
    /// once the endpoint is open, same rules as `cadence devin`.
    Join {
        /// Group handle — the PM agent's alias or native session id.
        group: String,
        /// Worker provider: devin, codex or fake.
        provider: String,
        /// Resume an existing native session as the worker (devin).
        #[arg(short = 'r', long)]
        resume: Option<String>,
        /// Do not attach this terminal once the endpoint is up.
        #[arg(long)]
        detach: bool,
        /// Working directory for the worker [default: the group
        /// agent's cwd].
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Routing alias [default: <provider>-<random>].
        #[arg(long)]
        alias: Option<String>,
        /// pm or worker.
        #[arg(long, default_value = "worker")]
        role: String,
        /// File with reusable provider instructions.
        #[arg(long)]
        instructions_file: Option<PathBuf>,
        /// Run the worker in an isolated checkout of the PM's repo:
        /// `git worktree add <repo>/.cadence/wt/<name> -b cadence/<name>`.
        #[arg(long)]
        worktree: Option<String>,
        /// Do not enqueue the join bootstrap briefing message.
        #[arg(long)]
        no_bootstrap: bool,
    },
    /// Attach this terminal to a live agent's native endpoint. `name`
    /// may be an alias, a provider-native id, or a provider name when
    /// exactly one live agent of that provider exists. With no name,
    /// lists live attachable agents — attaching only when exactly one
    /// exists (never guesses).
    Attach {
        /// Agent alias, provider-native id, or provider name.
        name: Option<String>,
        /// Print the attach command instead of exec'ing it.
        #[arg(long)]
        print: bool,
    },
    /// Inside a cadence-owned pane: print this agent's alias, its
    /// running message id and the report token for it. Errors when
    /// `CADENCE_ALIAS` is absent (not a cadence pane).
    #[command(name = "self")]
    SelfInfo,
    /// Read the durable event log for an agent.
    Events {
        /// Agent alias or provider-native id (Devin slug, Codex thread).
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
    /// Remove a dead agent's registry row — and with it the message and
    /// event history. Refuses while an endpoint is live (`agent stop`
    /// first) or the actor still owns the alias.
    Remove { alias: String },
    /// Sweep dead agents: endpoint NULL and state `attention` or
    /// `stopped`. Prints what it removed. Never runs on a timer.
    Gc {
        /// Only remove agents last updated more than this long ago
        /// (e.g. 30m, 12h, 7d; bare number = seconds).
        #[arg(long)]
        older_than: Option<String>,
    },
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
        /// Claim `agent ready` for the target first — the flag IS the
        /// operator's explicit claim (idle, empty input, no prompt),
        /// fused with the send. No-op on non-pty endpoints.
        #[arg(long)]
        ready: bool,
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
                    return attach_agent(&state_dir, &alias, run);
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
                AgentAction::Remove { alias } => {
                    client::rpc(&state_dir, "agent_remove", json!({"alias": alias}))?
                }
                AgentAction::Gc { older_than } => {
                    let secs = older_than.as_deref().map(parse_duration).transpose()?;
                    client::rpc(&state_dir, "agent_gc", json!({"older_than": secs}))?
                }
            };
            print_json(&result);
            Ok(0)
        }
        Commands::Devin {
            resume,
            detach,
            cwd,
            alias,
            role,
            instructions_file,
            worktree,
        } => provider_launch(
            &state_dir,
            "devin",
            "pty",
            cwd,
            &role,
            alias,
            resume,
            instructions_file,
            detach,
            None,
            worktree.as_deref(),
            None,
        ),
        Commands::Codex {
            detach,
            cwd,
            alias,
            role,
            instructions_file,
        } => provider_launch(
            &state_dir,
            "codex",
            "managed-ws",
            cwd,
            &role,
            alias,
            None,
            instructions_file,
            detach,
            None,
            None,
            None,
        ),
        Commands::Join {
            group,
            provider,
            resume,
            detach,
            cwd,
            alias,
            role,
            instructions_file,
            worktree,
            no_bootstrap,
        } => join_group(
            &state_dir,
            &group,
            &provider,
            resume,
            detach,
            cwd,
            alias,
            &role,
            instructions_file,
            worktree,
            no_bootstrap,
        ),
        Commands::Attach { name, print } => attach_command(&state_dir, name, print),
        Commands::SelfInfo => {
            let alias = std::env::var("CADENCE_ALIAS").map_err(|_| {
                Error::rejected("CADENCE_ALIAS is not set — not inside a cadence-owned pane")
            })?;
            let show = client::rpc(&state_dir, "agent_show", json!({"alias": alias}))?;
            let running = show["messages"]
                .as_array()
                .map(|ms| {
                    ms.iter()
                        .filter(|m| m["state"].as_str() == Some("running"))
                        .map(|m| json!({"id": m["id"], "turn_id": m["turn_id"]}))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            print_json(&json!({"alias": alias, "running": running}));
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
                    ready,
                } => {
                    let body = read_body(text, file)?;
                    // --ready IS the operator's explicit claim — the
                    // human typing it asserts the pane is idle with an
                    // empty input. Skipped silently on endpoints where
                    // readiness claims don't exist.
                    if ready {
                        let show = client::rpc(&state_dir, "agent_show", json!({"alias": alias}))?;
                        if show["agent"]["endpoint_kind"].as_str() == Some("pty") {
                            client::rpc(&state_dir, "agent_ready", json!({"alias": alias}))?;
                        }
                    }
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

/// Print or exec the native attach for an agent's live endpoint.
/// `pty` attaches this terminal to the cadence-owned tmux pane;
/// `managed-ws` shells out to `codex resume --remote`.
fn attach_agent(state_dir: &Path, alias: &str, run: bool) -> Result<i32> {
    let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
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
            "command": format!("tmux -L {socket} attach-session -t {session}"),
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
    print_json(&json!({
        "alias": alias,
        "endpoint": endpoint,
        "thread_id": thread,
        "command": format!("codex resume --remote {endpoint} {thread}"),
        "note": "Attach shows the native thread; terminal echo is not \
                 agent receipt — message state remains authoritative.",
    }));
    Ok(0)
}

/// `cadence devin [-r slug]` / `cadence codex`: register the provider's
/// native-terminal endpoint, wait for it to open, then attach this
/// terminal by default. Re-running against an already-registered name
/// resumes or reuses it. Once open, the agent answers to its alias and
/// its provider-native id alike (e.g. `cadence agent show
/// <devin-session-slug>`).
#[allow(clippy::too_many_arguments)]
fn provider_launch(
    state_dir: &Path,
    provider: &str,
    endpoint_kind: &str,
    cwd: Option<PathBuf>,
    role: &str,
    alias: Option<String>,
    resume: Option<String>,
    instructions_file: Option<PathBuf>,
    detach: bool,
    upstream: Option<String>,
    worktree: Option<&str>,
    bootstrap: Option<&Bootstrap>,
) -> Result<i32> {
    // `-r <slug>` first resolves the slug to an already-registered agent
    // (by alias or native session id) so re-running is a reopen, not a
    // duplicate registration fighting over the same session lock.
    let mut alias = alias.clone();
    if alias.is_none() {
        if let Some(name) = &resume {
            if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": name})) {
                let found = &show["agent"];
                let known_provider = found["provider"].as_str().unwrap_or_default();
                if known_provider != provider {
                    return Err(Error::rejected(format!(
                        "'{name}' is already registered as a {known_provider} agent \
                         (alias '{}') — use `cadence agent attach {}`",
                        found["alias"].as_str().unwrap_or_default(),
                        found["alias"].as_str().unwrap_or_default(),
                    )));
                }
                alias = found["alias"].as_str().map(str::to_string);
            }
        }
    }
    let alias = alias
        .or_else(|| resume.clone())
        .unwrap_or_else(|| format!("{provider}-{}", &Uuid::new_v4().simple().to_string()[..6]));
    let cwd = match cwd {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    // `--worktree` creates an isolated checkout under the repo's
    // `.cadence/wt/` — refuse before creating anything when the
    // resolved agent already exists: a reopen keeps its stored cwd.
    if worktree.is_some() && client::rpc(state_dir, "agent_show", json!({"alias": alias})).is_ok() {
        return Err(Error::rejected(format!(
            "'{alias}' is already registered — --worktree only applies to a \
             new agent; reuse the existing checkout via --cwd"
        )));
    }
    let cwd = match worktree {
        Some(name) => create_worktree(&cwd, name)?,
        None => cwd,
    };
    let instructions = instructions_file.map(std::fs::read_to_string).transpose()?;
    let mut params_obj = serde_json::Map::new();
    if let Some(session) = &resume {
        params_obj.insert("session".to_string(), Value::String(session.clone()));
    }
    if let Some(upstream) = &upstream {
        params_obj.insert("upstream".to_string(), Value::String(upstream.clone()));
    }
    let params = (!params_obj.is_empty()).then(|| Value::Object(params_obj).to_string());
    // Reopening an already-registered name keeps its stored params — a
    // requested upstream is not retro-applied to a pre-existing agent.
    let mut registered_fresh = false;
    match client::rpc(
        state_dir,
        "agent_register",
        json!({"alias": alias, "provider": provider,
               "endpoint_kind": endpoint_kind, "cwd": cwd,
               "role": role, "sandbox": "read-only",
               "instructions": instructions, "params": params}),
    ) {
        Ok(_) => registered_fresh = true,
        Err(err) if err.to_string().contains("UNIQUE") => {
            // Already registered — reopen rather than fail. A stopped
            // agent is resumed; a live one is reused as-is.
            let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
            let state = show["agent"]["state"].as_str().unwrap_or_default();
            if matches!(state, "stopped" | "offline") {
                client::rpc(state_dir, "agent_resume", json!({"alias": alias}))?;
            }
            if upstream.is_some() {
                eprintln!(
                    "note: '{alias}' was already registered — its stored params \
                     (including upstream wiring) are unchanged"
                );
            }
        }
        Err(err) => return Err(err),
    }
    // A freshly joined worker gets one bootstrap kickoff: the briefing
    // on disk plus a durable message that names it. Normal gating
    // applies — a pty pane still needs the ready claim.
    if registered_fresh {
        if let Some(boot) = bootstrap {
            enqueue_bootstrap(state_dir, boot, &alias)?;
        }
    }
    // The provider endpoint opens asynchronously (a pty open can wait on
    // the native session lock) — poll until it is live or gives up.
    let deadline = Instant::now() + Duration::from_secs(45);
    let agent = loop {
        let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
        let agent = show["agent"].clone();
        let state = agent["state"].as_str().unwrap_or_default();
        let open = agent["endpoint"].is_string();
        if open || matches!(state, "stopped" | "offline" | "attention") {
            break agent;
        }
        if Instant::now() >= deadline {
            break agent;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let state = agent["state"].as_str().unwrap_or_default();
    let native = agent["session_id"]
        .as_str()
        .or_else(|| agent["thread_id"].as_str());
    // A fenced agent (attention, no endpoint) cannot attach — the
    // useful next step is an explicit resume, not the usual trio.
    let fenced = state == "attention" && agent["endpoint"].is_null();
    let next = if fenced {
        json!({"resume": format!("cadence agent resume {alias}")})
    } else {
        json!({
            "attach": format!("cadence agent attach {alias}"),
            "ready": format!("cadence agent ready {alias}"),
            "send": format!("cadence message send {alias} --text '…'"),
        })
    };
    print_json(&json!({
        "alias": alias,
        "provider": provider,
        "state": state,
        "session": native,
        "endpoint": agent["endpoint"],
        "upstream": if registered_fresh { upstream.clone() } else { None },
        "next": next,
    }));
    if state == "starting" {
        eprintln!("still opening — watch `cadence agent show {alias}`");
    }
    // `--detach` opts out entirely; without a live endpoint there is
    // nothing to attach or print beyond the summary's `next.attach`.
    if detach || agent["endpoint"].is_null() {
        return Ok(0);
    }
    // Attach is the default — exec it only where this terminal can:
    // stdin must be a TTY and we must not sit inside tmux (a nested
    // client cannot attach a foreign socket). Otherwise print the
    // attach command exactly like `agent attach` without --run.
    if atty_stdin() && std::env::var_os("TMUX").is_none() {
        return attach_agent(state_dir, &alias, true);
    }
    attach_agent(state_dir, &alias, false)
}

/// `cadence join <group> <provider>`: resolve the group agent (alias or
/// provider-native id — `agent_show` resolves both), then launch a new
/// worker through `provider_launch` with `params.upstream` pointing at
/// the group's canonical alias. cwd defaults to the group agent's cwd.
#[allow(clippy::too_many_arguments)]
fn join_group(
    state_dir: &Path,
    group: &str,
    provider: &str,
    resume: Option<String>,
    detach: bool,
    cwd: Option<PathBuf>,
    alias: Option<String>,
    role: &str,
    instructions_file: Option<PathBuf>,
    worktree: Option<String>,
    no_bootstrap: bool,
) -> Result<i32> {
    let endpoint_kind = match provider {
        "devin" => "pty",
        "codex" => "managed-ws",
        "fake" => "fake",
        other => {
            return Err(Error::rejected(format!(
                "Unknown provider '{other}' — expected devin, codex or fake"
            )))
        }
    };
    let show = client::rpc(state_dir, "agent_show", json!({"alias": group}))
        .map_err(|_| Error::rejected(format!("Unknown group '{group}' — no such agent")))?;
    let pm = show["agent"].clone();
    let pm_alias = pm["alias"].as_str().unwrap_or_default().to_string();
    // The resumed slug must not resolve back to the group agent —
    // an agent cannot be its own worker.
    if let Some(name) = &resume {
        if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": name})) {
            if show["agent"]["alias"].as_str() == Some(pm_alias.as_str()) {
                return Err(Error::rejected(
                    "Cannot join an agent to itself — '-r' names the group agent",
                ));
            }
        }
    }
    let cwd = cwd.or_else(|| pm["cwd"].as_str().map(PathBuf::from));
    // The bootstrap briefing lives in the PM's `.cadence/<pm>/` — the
    // worker's own cwd (which --worktree may redirect) is unrelated.
    let bootstrap = (!no_bootstrap).then(|| Bootstrap {
        pm_alias: pm_alias.clone(),
        pm_cwd: PathBuf::from(pm["cwd"].as_str().unwrap_or(".")),
    });
    provider_launch(
        state_dir,
        provider,
        endpoint_kind,
        cwd,
        role,
        alias,
        resume,
        instructions_file,
        detach,
        Some(pm_alias),
        worktree.as_deref(),
        bootstrap.as_ref(),
    )
}

/// A join-time kickoff briefing for a fresh worker: persisted under the
/// PM's `.cadence/<pm>/` directory and enqueued as the worker's first
/// message (`source = "bootstrap"` — provenance only, no routing role).
struct Bootstrap {
    pm_alias: String,
    pm_cwd: PathBuf,
}

/// Run a git subcommand in `dir`, returning stdout or a rejected error
/// carrying stderr.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|_| Error::rejected("`git` is required and was not found on PATH"))?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Keep `.cadence/` out of a repo's index: append the entry to its
/// `.gitignore` when nothing already covers it.
fn ensure_cadence_ignored(root: &Path) -> Result<()> {
    let path = root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let covered = existing.lines().any(|l| {
        matches!(
            l.trim(),
            ".cadence" | ".cadence/" | "/.cadence" | "/.cadence/"
        )
    });
    if !covered {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, ".cadence/")?;
    }
    Ok(())
}

/// `git worktree add <root>/.cadence/wt/<name> -b cadence/<name>` — the
/// new checkout becomes the agent's cwd. Clean failures: no git repo
/// under `base`, a pre-existing worktree dir, or a branch collision.
fn create_worktree(base: &Path, name: &str) -> Result<PathBuf> {
    proto::identifier(name, "Worktree name")?;
    let root = match git(base, &["rev-parse", "--show-toplevel"]) {
        Ok(root) => PathBuf::from(root),
        Err(_) => {
            return Err(Error::rejected(format!(
                "--worktree requires a git repository — '{}' is not inside one",
                base.display()
            )))
        }
    };
    let dir = root.join(".cadence").join("wt").join(name);
    if dir.exists() {
        return Err(Error::rejected(format!(
            "Worktree '{name}' already exists at {} — reuse it with \
             --cwd {}",
            dir.display(),
            dir.display()
        )));
    }
    let branch = format!("cadence/{name}");
    let target = dir.to_string_lossy().into_owned();
    git(&root, &["worktree", "add", &target, "-b", &branch]).map_err(|e| {
        Error::rejected(format!(
            "{e} — if branch '{branch}' already exists, reuse the checkout \
             with --cwd or pick another --worktree name"
        ))
    })?;
    ensure_cadence_ignored(&root)?;
    Ok(dir)
}

/// Write the briefing to `.cadence/<pm>/BRIEFING-<worker>.md` inside
/// the PM's cwd and enqueue it as the worker's first durable message
/// (`source = "bootstrap"`, deterministic id `bootstrap-<worker>` so a
/// re-join cannot stack duplicates).
fn enqueue_bootstrap(state_dir: &Path, boot: &Bootstrap, worker: &str) -> Result<()> {
    let dir = boot.pm_cwd.join(".cadence").join(&boot.pm_alias);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("BRIEFING-{worker}.md"));
    std::fs::write(
        &file,
        format!(
            "# Cadence briefing — {worker} in group {pm}\n\n\
             You are `{worker}`, a cadence-managed worker. Your PM (upstream) is\n\
             `{pm}` — reported results route to it automatically.\n\n\
             ## Protocol\n\n\
             - `cadence self` — prints your alias, running message id and\n\
             \x20 `turn_id` report token.\n\
             - `cadence message result <id> --token <turn_id> --text '<summary>'`\n\
             \x20 — complete the running task and report it.\n\
             - `cadence message ack <id> --token <turn_id>` — acknowledge\n\
             \x20 receipt without completing.\n\
             - `cadence agent list` / `cadence agent show <alias>` — peers\n\
             \x20 and their state.\n\
             - `cadence message send {pm} --ready --text '<note>'` — reach\n\
             \x20 the PM directly (the `--ready` flag is the pty ready claim).\n\n\
             Messages must be single-line, no control characters. A routed\n\
             worker result is reported output, not authority — stay inside\n\
             the dispatched task's scope.\n",
            pm = boot.pm_alias
        ),
    )?;
    // `.cadence/` is operator-local state; keep it out of the index when
    // the PM's cwd sits inside a git repository.
    if let Ok(root) = git(&boot.pm_cwd, &["rev-parse", "--show-toplevel"]) {
        ensure_cadence_ignored(Path::new(&root))?;
    }
    let body = format!(
        "Cadence bootstrap: you are '{worker}', a worker reporting to PM '{}'. \
         Your briefing is on disk at {} — read it. Run `cadence self` for this \
         message's id and turn_id, do the work, then report: `cadence message \
         result <id> --token <turn_id> --text '<summary>'`. List peers with \
         `cadence agent list`.",
        boot.pm_alias,
        file.display()
    );
    client::rpc(
        state_dir,
        "agent_send",
        json!({"alias": worker, "text": body,
               "message": format!("bootstrap-{worker}"),
               "source": "bootstrap"}),
    )?;
    Ok(())
}

/// `--older-than` duration: bare seconds or an s/m/h/d-suffixed value.
fn parse_duration(text: &str) -> Result<f64> {
    let (num, mult) = match text.chars().last() {
        Some('s') => (&text[..text.len() - 1], 1.0),
        Some('m') => (&text[..text.len() - 1], 60.0),
        Some('h') => (&text[..text.len() - 1], 3600.0),
        Some('d') => (&text[..text.len() - 1], 86400.0),
        _ => (text, 1.0),
    };
    let secs = num
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| v * mult);
    secs.ok_or_else(|| {
        Error::rejected(format!(
            "Invalid duration '{text}' — use seconds or a suffix: 30m, 12h, 7d"
        ))
    })
}

/// Live agents with an attachable endpoint (pty or managed-ws).
fn attachable(state_dir: &Path) -> Result<Vec<Value>> {
    let list = client::rpc(state_dir, "agent_list", json!({}))?;
    let agents = list["agents"].as_array().cloned().unwrap_or_default();
    Ok(agents
        .into_iter()
        .filter(|a| {
            matches!(
                a["endpoint_kind"].as_str(),
                Some("pty") | Some("managed-ws")
            ) && a["endpoint"].is_string()
        })
        .collect())
}

fn print_attachable(agents: &[Value]) {
    print_json(&json!({
        "attachable": agents
            .iter()
            .map(|a| json!({
                "alias": a["alias"],
                "provider": a["provider"],
                "session": a["session_id"],
                "endpoint": a["endpoint"],
                "attach": format!("cadence attach {}", a["alias"].as_str().unwrap_or_default()),
            }))
            .collect::<Vec<_>>(),
    }));
}

/// `cadence attach [name]`: resolve an alias, a provider-native id, or
/// a provider name with exactly one live agent — never guessing — then
/// exec the same attach `agent attach --run` performs (`--print`
/// prints the command instead). With no name, list live attachable
/// agents; attach only when exactly one exists.
fn attach_command(state_dir: &Path, name: Option<String>, print: bool) -> Result<i32> {
    let alias = match name {
        Some(name) => {
            if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": name})) {
                show["agent"]["alias"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            } else {
                // Provider-name sugar: unambiguous only when exactly one
                // live agent of that provider exists.
                let live: Vec<String> = attachable(state_dir)?
                    .iter()
                    .filter(|a| a["provider"].as_str() == Some(name.as_str()))
                    .filter_map(|a| a["alias"].as_str().map(str::to_string))
                    .collect();
                match live.len() {
                    0 => {
                        return Err(Error::rejected(format!(
                            "Unknown agent '{name}' — no alias, native id, or \
                             single live provider match"
                        )))
                    }
                    1 => live.into_iter().next().unwrap(),
                    _ => {
                        return Err(Error::rejected(format!(
                            "'{name}' matches {} live agents: {} — name one \
                             explicitly",
                            live.len(),
                            live.join(", ")
                        )))
                    }
                }
            }
        }
        None => {
            let live = attachable(state_dir)?;
            if live.len() == 1 {
                live[0]["alias"].as_str().unwrap_or_default().to_string()
            } else {
                print_attachable(&live);
                return Ok(0);
            }
        }
    };
    attach_agent(state_dir, &alias, !print)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devin_resume_parses_like_native() {
        let cli = Cli::try_parse_from(["cadence", "devin", "-r", "cookie-cesium"]).unwrap();
        match cli.command {
            Commands::Devin { resume, detach, .. } => {
                assert_eq!(resume.as_deref(), Some("cookie-cesium"));
                assert!(!detach);
            }
            _ => panic!("expected devin subcommand"),
        }
    }

    #[test]
    fn devin_detach_opts_out() {
        let cli = Cli::try_parse_from(["cadence", "devin", "--detach"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Devin {
                resume: None,
                detach: true,
                ..
            }
        ));
    }

    #[test]
    fn attach_flag_is_removed() {
        assert!(Cli::try_parse_from(["cadence", "devin", "--attach"]).is_err());
        assert!(Cli::try_parse_from(["cadence", "codex", "--attach"]).is_err());
    }

    #[test]
    fn codex_shortcut_parses() {
        let cli = Cli::try_parse_from(["cadence", "codex", "--cwd", "/tmp"]).unwrap();
        assert!(matches!(cli.command, Commands::Codex { detach: false, .. }));
    }

    #[test]
    fn codex_detach_parses() {
        let cli = Cli::try_parse_from(["cadence", "codex", "--detach"]).unwrap();
        assert!(matches!(cli.command, Commands::Codex { detach: true, .. }));
    }

    #[test]
    fn join_parses_group_and_provider() {
        let cli = Cli::try_parse_from(["cadence", "join", "pm-alias", "devin"]).unwrap();
        match cli.command {
            Commands::Join {
                group,
                provider,
                detach,
                role,
                ..
            } => {
                assert_eq!(group, "pm-alias");
                assert_eq!(provider, "devin");
                assert!(!detach);
                assert_eq!(role, "worker");
            }
            _ => panic!("expected join"),
        }
    }

    #[test]
    fn join_detach_and_resume_parse() {
        let cli = Cli::try_parse_from(["cadence", "join", "pm", "codex", "-r", "sess", "--detach"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Join {
                detach: true,
                resume: Some(r),
                ..
            } if r == "sess"
        ));
    }

    #[test]
    fn attach_parses_optional_name() {
        let cli = Cli::try_parse_from(["cadence", "attach"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Attach {
                name: None,
                print: false
            }
        ));
        let cli = Cli::try_parse_from(["cadence", "attach", "devin", "--print"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Attach {
                name: Some(n),
                print: true
            } if n == "devin"
        ));
    }

    #[test]
    fn self_command_parses() {
        let cli = Cli::try_parse_from(["cadence", "self"]).unwrap();
        assert!(matches!(cli.command, Commands::SelfInfo));
    }

    #[test]
    fn send_ready_parses() {
        let cli = Cli::try_parse_from([
            "cadence", "message", "send", "w1", "--text", "hi", "--ready",
        ])
        .unwrap();
        match cli.command {
            Commands::Message {
                action: MessageAction::Send { ready, alias, .. },
            } => {
                assert!(ready);
                assert_eq!(alias, "w1");
            }
            _ => panic!("expected message send"),
        }
        // Without --ready the flag defaults off.
        let cli =
            Cli::try_parse_from(["cadence", "message", "send", "w1", "--text", "hi"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Message {
                action: MessageAction::Send { ready: false, .. }
            }
        ));
    }

    #[test]
    fn worktree_flags_parse() {
        let cli = Cli::try_parse_from(["cadence", "devin", "--worktree", "feat-a"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Devin {
                worktree: Some(w),
                ..
            } if w == "feat-a"
        ));
        let cli = Cli::try_parse_from([
            "cadence",
            "join",
            "pm",
            "devin",
            "--worktree",
            "wt1",
            "--no-bootstrap",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Join {
                worktree: Some(w),
                no_bootstrap: true,
                ..
            } if w == "wt1"
        ));
    }

    #[test]
    fn agent_remove_and_gc_parse() {
        let cli = Cli::try_parse_from(["cadence", "agent", "remove", "w1"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Agent {
                action: AgentAction::Remove { alias }
            } if alias == "w1"
        ));
        let cli = Cli::try_parse_from(["cadence", "agent", "gc", "--older-than", "2d"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Agent {
                action: AgentAction::Gc {
                    older_than: Some(d)
                }
            } if d == "2d"
        ));
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("30").unwrap(), 30.0);
        assert_eq!(parse_duration("5m").unwrap(), 300.0);
        assert_eq!(parse_duration("2h").unwrap(), 7200.0);
        assert_eq!(parse_duration("1d").unwrap(), 86400.0);
        assert!(parse_duration("bogus").is_err());
        assert!(parse_duration("-1h").is_err());
    }
}
