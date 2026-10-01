// CAD-535: CLI surface — clap definitions, dispatch, shared helpers.
// All code here is moved verbatim from src/main.rs.
// CAD-984: the `Commands` enum lives in `commands.rs`; `run()` and the
// verb dispatch live in `action_dispatch.rs` and are re-exported above.

mod action_dispatch;
mod agent;
mod agent_uid;
mod app;
mod attach;
mod audit;
mod backup;
mod briefing;
mod build_slot;
mod claude;
mod codex;
mod commands;
mod confine;
mod connection;
mod cursor;
mod daemon;
mod delivery;
mod devin;
mod dispatch;
mod doctor;
mod events;
mod export;
mod help;
mod idea;
mod inbox;
mod intake;
mod interrupt;
mod issue;
mod job;
mod join;
mod master;
mod mcp_agent;
mod mcp_permission;
mod memory;
mod message;
mod milestone;
mod monitor;
mod overview;
mod plan;
mod platform;
mod project;
mod remote_result;
mod report;
mod restore;
mod resume;
mod review;
mod rollout;
mod sandbox;
mod secret;
mod self_info;
mod send;
mod session;
mod setup;
mod skill;
mod status;
mod stop;
mod test_cmd;
#[cfg(test)]
mod tests;
mod thread;
mod ui;
mod update;
#[cfg(all(test, feature = "test-seam"))]
mod update_recovery_tests;
mod upgrade;
mod wiki;
mod workflow;

pub(crate) use action_dispatch::run;
use agent::AgentAction;
use agent_uid::AgentUidAction;
use app::AppAction;
use attach::attach_agent;
use audit::AuditAction;
use briefing::{brief_agent, BriefMode};
#[cfg(test)]
use briefing::{cloud_session_prompt, ensure_agents_block, AGENTS_BEGIN, AGENTS_END};
use build_slot::BuildSlotAction;
use cadence_agent::adapter::registry;
use cadence_agent::client;
use cadence_agent::error::Error;
use cadence_agent::error::Result;
use clap::Parser;
use clap::Subcommand;
use commands::Commands;
use connection::ConnectionAction;
use daemon::DaemonAction;
use delivery::DeliveryAction;
pub(crate) use devin::{apply_cloud_params, insert_devin_cloud_params, split_cloud_params};
use idea::IdeaAction;
use inbox::InboxAction;
use intake::IntakeAction;
use job::JobAction;
use master::MasterAction;
use message::MessageAction;
use monitor::MonitorAction;
use plan::PlanAction;
use platform::PlatformAction;
use project::ProjectCmd;
use remote_result::RemoteAction;
use report::ReportAction;
use rollout::RolloutAction;
use secret::SecretAction;
use serde_json::json;
use serde_json::Value;
use session::SessionAction;
use skill::SkillAction;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::time::Instant;
use thread::ThreadAction;
use update::UpdateAction;
use uuid::Uuid;
use workflow::WorkflowAction;

#[derive(Parser)]
#[command(
    name = "cadence",
    about = "Local controller for coding agents",
    // `0.1.0+<build commit>` — semver build metadata, `unknown` when
    // the build ran outside a git checkout.
    version = concat!(env!("CARGO_PKG_VERSION"), "+", env!("CADENCE_BUILD_COMMIT"))
)]
pub(crate) struct Cli {
    /// Runtime state directory (socket, database, logs).
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum ReviewAction {
    /// Print historical sightings without modifying the flake ledger.
    Flakes {
        /// Exact test name to select.
        #[arg(long)]
        test: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum AuthAction {
    /// Verify with the issuer; CADENCE_TOKEN takes precedence and is never persisted.
    Status {
        #[arg(long)]
        issuer: Option<String>,
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        auth_dir: Option<PathBuf>,
    },
    /// Remove the local credential; server revocation remains an AgenticOS action.
    Logout {
        #[arg(long)]
        auth_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub(crate) enum TaskAction {
    /// Add a draft task to an open job.
    Add {
        job: String,
        /// Task id (global identifier charset; default `<job>-t<n>`).
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Assignee — the PM itself or a member of its group.
        #[arg(long)]
        assignee: Option<String>,
        /// Task-level spec (falls back to the job's).
        #[arg(long)]
        spec: Option<PathBuf>,
        /// Acceptance criteria — inline text or a path.
        #[arg(long)]
        accept: Option<String>,
        /// Worktree name (`.cadence/wt/<name>`) — the scope claim.
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Revision the work starts from.
        #[arg(long)]
        base_sha: Option<String>,
    },
    /// Show one task: row, live kickoff, attached messages, verdicts.
    Show { task: String },
    /// Record the reported commit manually — the repair path when a
    /// kickoff completed without `SHA:`/`--sha`. Review-state tasks
    /// only; never overwrites a bound SHA.
    Sha { task: String, sha: String },
    /// Mark a task unrecoverable (PM/operator decision).
    Fail {
        task: String,
        #[arg(long)]
        reason: String,
    },
    /// Reopen a blocked/verified/failed task to draft — the operator or
    /// the job's own PM, derived from the calling process (CAD-373).
    Reopen { task: String },
    /// Cancel a task; a still-queued kickoff is cancelled with it.
    Cancel { task: String },
}

/// Terminal state an operator reconcile may record.
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum ReconcileStatus {
    /// The outcome was never learned — move on; never auto-replayed.
    Interrupted,
    /// The turn is confirmed finished; `reply_to` routes its result.
    Completed,
    /// The turn is confirmed failed; `reply_to` routes its result.
    Failed,
}

impl ReconcileStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

pub(crate) fn read_body(text: Option<String>, file: Option<PathBuf>) -> Result<String> {
    read_body_capped(text, file, u64::MAX)
}

/// `read_body` with a byte bound on the *read* — a giant `--file` or
/// stdin paste is refused before it is fully buffered (`report`
/// passes [`cadence_agent::issue::report::BODY_MAX`]; the cap error
/// itself comes from `report::file`).
pub(crate) fn read_body_capped(
    text: Option<String>,
    file: Option<PathBuf>,
    max: u64,
) -> Result<String> {
    // Read one byte beyond a bounded body so the caller can reject an
    // oversized input without buffering it in full. `u64::MAX` is the
    // uncapped send path; saturating keeps that path from overflowing
    // while still being effectively unlimited for any file or stdin.
    let read_limit = max.saturating_add(1);
    if let Some(text) = text {
        return Ok(text);
    }
    // `--file -` is stdin, like no file at all (CAD-339: an agent whose
    // only tool is `cadence` pipes a heredoc).
    if let Some(file) = file.filter(|f| f.as_os_str() != "-") {
        let mut body = String::new();
        std::fs::File::open(&file)?
            .take(read_limit)
            .read_to_string(&mut body)?;
        return Ok(body);
    }
    if atty_stdin() {
        return Err(Error::rejected("Provide --text or --file"));
    }
    let mut body = String::new();
    std::io::stdin()
        .take(read_limit)
        .read_to_string(&mut body)?;
    Ok(body)
}

/// Body for `message result` / `message ack` / `done` `--text` / `--file`
/// (CAD-880, I5): a result needs exactly one; an ack takes either or
/// neither. `--file -` reads stdin capped at 4 MiB — the same bound as
/// `issue comment --file -` — and a terminal stdin is refused. A file
/// past the cap is refused rather than truncated.
pub(crate) fn read_result_body(
    text: Option<String>,
    file: Option<PathBuf>,
    required: bool,
) -> Result<Option<String>> {
    const MAX: u64 = 4 << 20;
    match (text, file) {
        (Some(_), Some(_)) => Err(Error::rejected("Provide --text or --file, not both")),
        (Some(t), None) => Ok(Some(t)),
        (None, Some(f)) => {
            let body = read_body_capped(None, Some(f), MAX)?;
            if body.len() as u64 > MAX {
                return Err(Error::rejected("message body is over 4 MiB"));
            }
            Ok(Some(body))
        }
        (None, None) if required => Err(Error::rejected("Provide --text or --file")),
        (None, None) => Ok(None),
    }
}

/// What `report_result_text` decided: the text to report, or an already
/// stored outcome that answers the retry with nothing more to send
/// (CAD-880: an already-reported turn resolves no default).
pub(crate) enum ReportReady {
    Send(String),
    Done(Value),
}
/// Body for an allowlisted master command that takes `--file`
/// (`plan propose`, `report file`, `report`, `master escalate`).
///
/// When [`cadence_agent::master::caller_is_master`] is set, `-` and a
/// missing path are refused with [`cadence_agent::master::NO_STDIN`]
/// before stdin is opened, and a real path goes through
/// [`cadence_agent::master::read_command_file`]. Other callers keep
/// the stdin pipe.
pub(crate) fn read_master_command_file(
    state_dir: &Path,
    file: Option<&Path>,
    max: u64,
) -> Result<String> {
    let path = file.filter(|f| f.as_os_str() != "-" && !f.as_os_str().is_empty());
    if cadence_agent::master::caller_is_master() {
        let Some(path) = path else {
            return Err(Error::rejected(cadence_agent::master::NO_STDIN));
        };
        return cadence_agent::master::read_command_file(state_dir, path, max);
    }
    read_body_capped(None, file.map(Path::to_path_buf), max)
}

/// `message result --report` (CAD-341): the result text with its
/// `Report: <path>` line. Order matters — nothing is filed until the
/// report validates locally AND the daemon's token/state gates pass
/// (`check`), and the report's task must be the issue the message's
/// task is bound to. A retry after a completed result files nothing:
/// the stored result text is re-sent so the daemon sees a duplicate.
pub(crate) fn report_result_text(
    state_dir: &Path,
    message: Option<&str>,
    token: Option<&str>,
    text: String,
    sha: Option<&str>,
    path: PathBuf,
) -> Result<ReportReady> {
    use cadence_agent::issue::task_report;
    let body = read_body_capped(None, Some(path), task_report::BODY_MAX as u64)?;
    let pm = cadence_agent::issue::Pm::open_default()?;
    let prepared = task_report::prepare(&pm, &body, None, None)?;
    let mut check_params = json!({"kind": "result",
           "text": text, "sha": sha, "check": true});
    if let Some(m) = message {
        check_params["message"] = json!(m);
    }
    if let Some(t) = token {
        check_params["token"] = json!(t);
    }
    let check = client::rpc(state_dir, "message_report", check_params)?;
    match check["issue"].as_str() {
        Some(bound) if bound == prepared.task() => {}
        Some(bound) => {
            let what = message.unwrap_or("your running turn");
            return Err(Error::rejected(format!(
                "Report task {} is not {bound}, the issue message {what} is bound to",
                prepared.task()
            )));
        }
        None => {
            let what = message
                .map(|m| format!("Message {m}"))
                .unwrap_or_else(|| "Your running turn".to_string());
            return Err(Error::rejected(format!(
                "{what} is not bound to an issue task — file the report \
                 with `cadence report file --task <ID>` instead"
            )));
        }
    }
    let with_report = |at: &str| format!("{}\n\nReport: {at}", text.trim_end());
    if check["state"] == "completed" {
        // CAD-880: an already-reported turn resolves no default — the
        // stored outcome answers the retry with nothing more to send.
        // The explicit path keeps its resend (the daemon judges the
        // duplicate), so only the id-less call short-circuits here.
        if message.is_none() {
            return Ok(ReportReady::Done(
                json!({"state": "completed", "duplicate": true}),
            ));
        }
        let stored = check["result_text"].as_str().unwrap_or_default();
        let prefix = with_report("");
        return Ok(ReportReady::Send(if stored.starts_with(&prefix) {
            stored.to_string()
        } else {
            text
        }));
    }
    let filed = task_report::store(&pm, &prepared, "")?;
    let _ = client::rpc_timeout(
        state_dir,
        "reports_changed",
        json!({}),
        std::time::Duration::from_secs(2),
    );
    Ok(ReportReady::Send(with_report(
        filed["path"].as_str().unwrap_or_default(),
    )))
}

pub(crate) fn atty_stdin() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

/// CAD-158 steering flags, shared by `send` and `message send`. Only
/// the operator or the recipient's PM may use them; the daemon derives
/// the caller from the connection.
#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct SteerArgs {
    /// Delivery rank. `urgent` is delivered ahead of every queued
    /// normal message at the next safe turn boundary — it never
    /// interrupts a running turn or an open approval; FIFO within a
    /// rank.
    #[arg(long, value_parser = ["normal", "urgent"])]
    priority: Option<String>,
    /// Replace these still-queued messages (comma-separated ids): in
    /// one transaction each is cancelled as "superseded by <new id>"
    /// and this message is queued. If any is not still queued, nothing
    /// changes.
    #[arg(long, value_delimiter = ',')]
    supersedes: Vec<String>,
}

/// Shared send path for `cadence send` and `cadence message send`:
/// resolve the body, apply the `--ready` operator claim on pty
/// endpoints, enqueue. Returns the RPC result plus a `pending` flag
/// (always false for send — kept for the shared call shape).
#[allow(clippy::too_many_arguments)]
pub(crate) fn send_message(
    state_dir: &Path,
    alias: &str,
    text: Option<String>,
    file: Option<PathBuf>,
    message: Option<String>,
    reply_to: Option<String>,
    ready: bool,
    force: bool,
    task: Option<String>,
    nudge: bool,
    steer: SteerArgs,
) -> Result<(Value, bool)> {
    let body = read_body(text, file)?;
    // --ready IS the operator's explicit claim — and the claim probes
    // the pane: a visibly busy endpoint refuses unless --force. Skipped
    // silently on endpoints where readiness claims don't exist. The
    // claim is attributed to CADENCE_ALIAS when sent from inside a pane.
    if ready {
        let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
        let agent = &show["agent"];
        if registry::ready_gate(
            agent["provider"].as_str().unwrap_or_default(),
            agent["endpoint_kind"].as_str().unwrap_or_default(),
        ) {
            let by = std::env::var("CADENCE_ALIAS").ok();
            client::rpc(
                state_dir,
                "agent_ready",
                json!({"alias": alias, "by": by, "force": force}),
            )?;
        }
    }
    let receipt = client::rpc(
        state_dir,
        "agent_send",
        json!({"alias": alias, "text": body,
               "message": message, "reply_to": reply_to,
               "task": task, "nudge": nudge,
               "priority": steer.priority,
               "supersedes": (!steer.supersedes.is_empty()).then_some(steer.supersedes)}),
    )?;
    // CAD-251: a stale mailbox still accepted the message — say so on
    // stderr so stdout stays the JSON receipt.
    if let Some(warning) = receipt["warning"].as_str() {
        eprintln!("warning: {warning}");
    }
    Ok((receipt, false))
}

/// Has the daemon released the state-dir singleton? `serve` holds an
/// exclusive `flock` on `cadence.lock` for its whole life; the kernel
/// drops it only when the process exits, so a successful non-blocking
/// lock probe is the exact "old daemon is gone" signal `daemon stop`
/// must wait for before a `daemon start` can win the same lock.
pub(crate) fn daemon_lock_free(state_dir: &Path) -> bool {
    use std::os::unix::io::AsRawFd;
    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .open(state_dir.join("cadence.lock"))
    else {
        // No lock file yet — no daemon ever owned this dir.
        return true;
    };
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    // The File drops here either way — a successful probe must not
    // keep the lock it was only testing.
    rc == 0
}

/// Poll until the daemon releases `cadence.lock`, bounded. Returns
/// false on timeout.
pub(crate) fn wait_daemon_exit(state_dir: &Path, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if daemon_lock_free(state_dir) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    daemon_lock_free(state_dir)
}

/// `daemon stop`: ask for shutdown, then wait until the process has
/// actually exited — the rpc returns while the daemon is still
/// draining actors, and an immediate `daemon start` would lose the
/// singleton race without this wait.
pub(crate) fn daemon_stop(state_dir: &Path) -> Result<i32> {
    let result = client::rpc(state_dir, "shutdown", json!({}))?;
    let exited = wait_daemon_exit(state_dir, 30);
    let mut out = result;
    out["exited"] = json!(exited);
    print_json(&out);
    if exited {
        Ok(0)
    } else {
        Err(Error::rejected(
            "daemon did not exit within 30s — it is still draining; \
             retry `daemon stop` or inspect daemon.log",
        ))
    }
}

/// CAD-503: can an in-flight turn on this agent still report? `stopped`,
/// `offline` and `attention` are settled no-actor states, and `dead` is
/// the daemon's own no-live-endpoint/no-pid verdict — a `running` or
/// `submitted` row under either is stale and can never finish, so waiting
/// on it is a deadlock (F19). `starting`/`stopping` still hold an actor
/// mid-transition — their rows stay live blockers.
pub(crate) fn stale_holder(agent: &Value) -> bool {
    match agent["state"].as_str().unwrap_or_default() {
        "stopped" | "offline" | "attention" => true,
        "starting" | "stopping" => false,
        _ => agent["dead"].as_bool().unwrap_or(false),
    }
}

/// One status line for the restart wait loops, split by whether waiting
/// can help: `busy` agents still holding the fleet (pty panes that probe
/// busy, actor agents with a `running`/`submitted` message on a live
/// actor) and `stale` turns — in-flight rows whose agent has no live
/// actor left to report them (CAD-503). Waiting on a stale turn is a
/// deadlock, so the caller refuses it (or carries it with
/// `--ignore-stale`) instead of blocking until the timeout.
pub(crate) fn busy_agents(
    state_dir: &Path,
    agents: &[Value],
) -> (Vec<String>, Vec<String>, Vec<(String, String)>) {
    let mut busy = Vec::new();
    let mut stale = Vec::new();
    let mut stale_pairs = Vec::new();
    for a in agents {
        let alias = a["alias"].as_str().unwrap_or_default();
        let provider = a["provider"].as_str().unwrap_or_default();
        let kind = a["endpoint_kind"].as_str().unwrap_or_default();
        if !registry::has_actor(provider, kind) {
            continue;
        }
        if kind == "pty" && a["endpoint"].is_string() {
            match client::rpc(state_dir, "agent_probe", json!({"alias": alias})) {
                Ok(probe) if !probe["idle"].as_bool().unwrap_or(false) => busy.push(format!(
                    "{alias}(busy: {})",
                    probe["reason"].as_str().unwrap_or("pane busy")
                )),
                _ => {}
            }
        }
        // In-flight turn on any actor endpoint — the pane probe can
        // read idle between paste and render, so the message row is
        // the authoritative in-flight signal.
        if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
            let inflight: Vec<&Value> = show["messages"]
                .as_array()
                .map(|ms| {
                    ms.iter()
                        .filter(|m| {
                            matches!(
                                m["state"].as_str().unwrap_or_default(),
                                "running" | "submitted"
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            if inflight.is_empty() {
                continue;
            }
            if stale_holder(a) {
                for m in inflight {
                    stale.push(format!(
                        "{alias}: {} ({})",
                        m["id"].as_str().unwrap_or("?"),
                        m["state"].as_str().unwrap_or("?")
                    ));
                    stale_pairs.push((
                        alias.to_string(),
                        m["id"].as_str().unwrap_or("?").to_string(),
                    ));
                }
            } else {
                busy.push(format!("{alias}(running message)"));
            }
        }
    }
    (busy, stale, stale_pairs)
}

/// The detached UI's `ui run` argv from /proc — restarting the board
/// keeps the host/port/dist/allow-hosts it was actually started with
/// rather than assuming the defaults.
pub(crate) fn ui_run_args(pid: i32) -> (String, u16, Option<std::path::PathBuf>, Vec<String>) {
    let mut host = "127.0.0.1".to_string();
    let mut port = 3010u16;
    let mut dist = None;
    let mut allow_hosts = Vec::new();
    let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return (host, port, dist, allow_hosts);
    };
    let args: Vec<String> = bytes
        .split(|b| *b == 0)
        .filter_map(|s| String::from_utf8(s.to_vec()).ok())
        .collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" if i + 1 < args.len() => host = args[i + 1].clone(),
            "--port" if i + 1 < args.len() => {
                port = args[i + 1].parse().unwrap_or(3010);
            }
            "--dist" if i + 1 < args.len() => {
                dist = Some(std::path::PathBuf::from(&args[i + 1]));
            }
            "--allow-host" if i + 1 < args.len() => allow_hosts.push(args[i + 1].clone()),
            _ => {}
        }
        i += 1;
    }
    (host, port, dist, allow_hosts)
}

/// CAD-424: the replacement daemon refuses to start over an interrupted
/// restore's leftovers, so a restart that shut the old one down first
/// would leave no daemon at all and the recovery only in daemon.log.
/// Refuse with the same recovery message before anything stops.
pub(crate) fn refuse_restart_over_leftovers(state_dir: &Path) -> Result<()> {
    cadence_agent::backup::refuse_interrupted_restore(state_dir).map_err(|error| {
        Error::rejected(format!(
            "daemon restart refused before shutdown; the running daemon is untouched: {error}"
        ))
    })
}

/// `daemon restart`: stop, wait for the process to exit (the
/// singleton lock is the truth), start, wait until every agent that
/// was live before settles out of `starting`/`offline`, then print a
/// before/after table. `--when-idle` gates the whole thing on a
/// quiet fleet first; `--ui` bounces the detached board server too.
/// CAD-503: a `running`/`submitted` row on a stopped or dead agent is
/// stale — no actor exists to report it, so waiting is a deadlock
/// (F19). `--when-idle` refuses such turns early with their remedy
/// instead of timing out; `--ignore-stale` carries them through.
pub(crate) fn daemon_restart(
    state_dir: &Path,
    when_idle: bool,
    timeout: u64,
    ignore_stale: bool,
    ui: bool,
    as_identity: Option<String>,
) -> Result<i32> {
    refuse_restart_over_leftovers(state_dir)?;
    let caller =
        cadence_agent::rollout::resolve_caller(as_identity.as_deref()).map_err(|error| {
            Error::rejected(format!("daemon restart refused before shutdown: {error}"))
        })?;
    let ticket = cadence_agent::rollout::begin_restart(state_dir, &caller)?;
    // A failed replacement can leave no daemon serving the socket.
    // Preserve semantic refusals, but let transport failure reach the
    // shutdown/lock checks below so rollback can start the old release.
    let (before, reachable) = match client::rpc_answer(state_dir, "agent_list", json!({})) {
        Ok(Ok(answer)) => (
            answer["agents"].as_array().cloned().unwrap_or_default(),
            true,
        ),
        Ok(Err(refused)) => return Err(refused),
        Err(_) => {
            // A lost response does not prove the daemon is down. Never
            // skip --when-idle on a live lock holder whose fleet
            // snapshot was unavailable.
            if !daemon_lock_free(state_dir) {
                return Err(Error::rejected(
                    "daemon owns the state-dir lock but does not answer the \
                     socket — inspect daemon.log before restarting",
                ));
            }
            (Vec::new(), false)
        }
    };
    // Stale `(alias, message_id)` pairs observed while waiting — a row
    // swept to `unknown` by the replacement store's recovery fenced a
    // turn without any `turn_adopt_refused` event to catch it.
    let mut stale_turns: Vec<(String, String)> = Vec::new();
    if when_idle && reachable {
        let deadline = Instant::now() + Duration::from_secs(timeout);
        let mut next_report = Instant::now();
        let mut stale_noted = false;
        loop {
            let agents = client::rpc(state_dir, "agent_list", json!({}))?["agents"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let (busy, stale, pairs) = busy_agents(state_dir, &agents);
            if !stale.is_empty() {
                if !ignore_stale {
                    return Err(Error::rejected(format!(
                        "{} stale turn(s) on stopped or dead agents can never \
                         report — restart aborted before touching anything: {}. \
                         Settle each with `cadence agent resume <alias>` (the \
                         report bound then retires it for reconcile) or \
                         `cadence agent remove --force <alias>` (settles it now, \
                         dropping the agent), or pass --ignore-stale to carry \
                         the stale rows through the restart",
                        stale.len(),
                        stale.join(", ")
                    )));
                }
                if !stale_noted {
                    eprintln!(
                        "when-idle: ignoring {} stale turn(s) on stopped or \
                         dead agents: {}",
                        stale.len(),
                        stale.join(", ")
                    );
                    stale_noted = true;
                }
                for p in pairs {
                    if !stale_turns.contains(&p) {
                        stale_turns.push(p);
                    }
                }
            }
            if busy.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(Error::rejected(format!(
                    "fleet still busy after {timeout}s — restart aborted \
                     before touching anything: {}",
                    busy.join(", ")
                )));
            }
            if Instant::now() >= next_report {
                eprintln!("when-idle: waiting on {}", busy.join(", "));
                next_report = Instant::now() + Duration::from_secs(30);
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    // --ui restarts the detached board server when one was running —
    // snapshot before the daemon goes down so a ui that dies mid-way
    // isn't "rediscovered".
    let ui_was_running = ui
        .then(|| cadence_agent::ui::detached_pid(state_dir))
        .flatten();
    // Stop: the shutdown rpc returns while the daemon drains; the
    // lock probe is the real exit. A daemon that was never running
    // (lock free, socket dead) skips straight to start.
    // Per-alias event cursors taken now bound what this restart
    // produced — paged forward after the restart they cannot miss a
    // `turn_adopted`/`turn_adopt_refused` to a busy agent's tail page,
    // and an older restart's events can never masquerade as this
    // one's.
    let mut event_cursors: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    // Aliases whose pre-stop tail read failed — the verdict cannot
    // bound their event history, so it must fail rather than silently
    // drop the check (CAD-694).
    let mut cursor_misses: Vec<String> = Vec::new();
    for a in &before {
        if a["endpoint_kind"].as_str() != Some("pty") {
            continue;
        }
        let Some(alias) = a["alias"].as_str().map(str::to_string) else {
            continue;
        };
        match client::rpc(
            state_dir,
            "agent_events",
            json!({"alias": alias, "tail": true}),
        ) {
            Ok(v) => {
                event_cursors.insert(alias, v["cursor"].as_i64().unwrap_or(0));
            }
            Err(_) => cursor_misses.push(alias),
        }
    }
    // The lease can be released, handed off, or expire while --when-idle
    // waits. Re-check immediately before shutdown and do not restart
    // when this caller no longer holds it.
    cadence_agent::rollout::recheck_restart(state_dir, &ticket)?;
    // --when-idle can wait for minutes; look again right before shutdown.
    refuse_restart_over_leftovers(state_dir)?;
    // CAD-384: a daemon that ANSWERS is running — a refusal (the caller
    // rule: not the operator, not a granted lease holder) aborts the
    // restart here, before anything is recorded. Only an unreachable
    // socket means "not running".
    let was_running = if reachable {
        match client::rpc_answer(state_dir, "shutdown", json!({})) {
            Ok(Ok(_)) => true,
            Ok(Err(refused)) => return Err(refused),
            Err(_) => false,
        }
    } else {
        // Do not shut down a daemon that appeared after the free-lock
        // proof: it supplied no fleet snapshot or idle proof. The lock
        // is checked again below before attempting an offline start.
        false
    };
    if !was_running && !daemon_lock_free(state_dir) {
        return Err(Error::rejected(
            "daemon owns the state-dir lock but does not answer the \
             socket — inspect daemon.log before restarting",
        ));
    }
    cadence_agent::rollout::note_restart_proceeded(state_dir, &ticket)?;
    if was_running && !wait_daemon_exit(state_dir, 30) {
        return Err(Error::rejected(
            "daemon did not exit within 30s — restart aborted; the old \
             process is still draining (see daemon.log)",
        ));
    }
    let started = client::daemon_start_as(state_dir, Some(&caller.identity))?;
    // CAD-694: the recovery-record verdict binds to the instance THIS
    // start spawned. Only a `started` receipt pid-matches its health
    // answer to the spawned child; an `already_running` receipt carries
    // whichever daemon held the socket (a concurrent restart's), so it
    // binds nothing: the restart fails closed on it, never clean on
    // another run's evidence.
    let started_instance = started_instance(&started);
    let start_unbound = started["state"].as_str() != Some("started");
    if start_unbound {
        eprintln!(
            "restart: the start answered {} instead of `started` — that daemon is not \
             the one this restart spawned, so its recovery evidence is not bound to \
             this restart",
            started["state"].as_str().unwrap_or("without a state")
        );
    }
    // Wait until every agent that was live before leaves the
    // transitional states — `starting` (actor up, endpoint not open)
    // and `offline` (actor exited under shutdown). Stopped and fenced
    // agents keep their state; the wait is about liveness settling.
    let live_before: Vec<String> = before
        .iter()
        .filter(|a| {
            !matches!(
                a["state"].as_str().unwrap_or_default(),
                "stopped" | "attention" | "offline"
            )
        })
        .filter_map(|a| a["alias"].as_str().map(str::to_string))
        .collect();
    let settle_deadline = Instant::now() + Duration::from_secs(120);
    let after = loop {
        let after = client::rpc(state_dir, "agent_list", json!({}))?["agents"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let unsettled = after.iter().any(|a| {
            let alias = a["alias"].as_str().unwrap_or_default();
            live_before.iter().any(|l| l == alias)
                && matches!(
                    a["state"].as_str().unwrap_or_default(),
                    "starting" | "offline"
                )
        });
        if !unsettled || Instant::now() >= settle_deadline {
            break after;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    if let Some(ui_pid) = ui_was_running {
        cadence_agent::ui::run_cli(
            state_dir,
            &cadence_agent::ui::UiAction::Stop {
                tailscale_off: false,
            },
        )?;
        // ui.json is the source of truth now; the /proc argv is only
        // the fallback for a server started before options persisted.
        let flags = if cadence_agent::ui::opts_present(state_dir) {
            cadence_agent::ui::UiFlags::default()
        } else {
            let (host, port, dist, allow_hosts) = ui_run_args(ui_pid);
            cadence_agent::ui::UiFlags {
                host: Some(host),
                port: Some(port),
                dist,
                allow_hosts,
                ..Default::default()
            }
        };
        // CAD-384: the board is the operator's, even when the rollout
        // owner restarts from its pane — a board that inherits the pane's
        // `CADENCE_ALIAS` fails operator proof on every operator write it
        // relays (`monitor_alert_ack`, `thread_send`, `model_defaults_set`).
        // The restart's caller was resolved above; nothing after this
        // reads the alias.
        std::env::remove_var("CADENCE_ALIAS");
        cadence_agent::ui::run_cli(
            state_dir,
            &cadence_agent::ui::UiAction::Start {
                flags,
                reset: false,
            },
        )?;
        println!("ui: restarted");
    }
    // Before/after table: state then, state now, and for pty agents
    // whether the pane pid survived and whether an in-flight turn was
    // re-adopted (`kept`) or fenced (`fenced`) by the hot restart. A
    // changed pane pid, a fenced turn, a new attention state, or an
    // unsettled previously live agent makes the command exit non-zero.
    // Existing fences stay visible without being blamed on this restart.
    let before_by_alias: std::collections::HashMap<&str, &Value> = before
        .iter()
        .filter_map(|a| a["alias"].as_str().map(|al| (al, a)))
        .collect();
    let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
    let mut bad = start_unbound;
    // CAD-694: the successor's recovery record is drain evidence the
    // cursors cannot always carry — a predecessor already dead gave
    // none, and non-pty endpoints were never scanned. The record is
    // bound to the instance this start returned; missing, malformed or
    // mismatched, a failed drain, or any fenced turn fails closed.
    // An unbound start already reported itself above; its record verdict
    // would only repeat that, so it is judged only for a spawned start.
    if !start_unbound {
        if let Some(reason) = recovery_record_verdict(state_dir, started_instance.as_deref()) {
            eprintln!("restart: {reason}");
            bad = true;
        }
    }
    for alias in &cursor_misses {
        eprintln!("restart: no pre-stop event cursor for {alias} — its verdict is unverified");
        bad = true;
    }
    let mut existing_fences = Vec::new();
    for a in &after {
        let alias = a["alias"].as_str().unwrap_or_default().to_string();
        let b = before_by_alias.get(alias.as_str());
        let before_state = b
            .and_then(|b| b["state"].as_str())
            .unwrap_or("-")
            .to_string();
        let after_state = a["state"].as_str().unwrap_or_default().to_string();
        let mut pane = "-".to_string();
        let mut turn = "-".to_string();
        if a["endpoint_kind"].as_str() == Some("pty") {
            let old = b.and_then(|b| b["pid"].as_u64()).unwrap_or(0);
            let new = a["pid"].as_u64().unwrap_or(0);
            if old == 0 && new == 0 {
                // no pane either side
            } else if old == new {
                pane = "same".to_string();
            } else {
                bad = true;
                pane = format!("CHANGED {old}→{new}");
            }
            // The adopt verdicts are events on the agent — page
            // forward from the cursor taken before the stop so only
            // this restart's events count and none can be missed. A
            // missing cursor means the pre-stop fetch failed; paging
            // from zero could pick up an older restart's verdicts, so
            // the column stays `-` instead.
            if let Some(cursor) = event_cursors.get(alias.as_str()).copied() {
                let mut seq = cursor;
                let mut kinds: Vec<String> = Vec::new();
                // A failed page leaves this restart's verdict unknown —
                // recorded and failed below, never silently skipped.
                let mut pages_failed = false;
                // Bounded: a restart's adopt verdicts land within a few
                // events; 20 pages of 100 is far past any real gap and
                // keeps a pathological event stream from looping.
                for _ in 0..20 {
                    let page = client::rpc(
                        state_dir,
                        "agent_events",
                        serde_json::json!({"alias": alias, "after": seq}),
                    );
                    let Ok(v) = page else {
                        pages_failed = true;
                        break;
                    };
                    let events = v["events"].as_array().cloned().unwrap_or_default();
                    let n = events.len();
                    for e in &events {
                        if let Some(k) = e["kind"].as_str() {
                            kinds.push(k.to_string());
                        }
                    }
                    seq = v["cursor"].as_i64().unwrap_or(seq);
                    if n < 100 {
                        break;
                    }
                }
                if pages_failed {
                    eprintln!(
                        "restart: event history unread for {alias} — its verdict is unverified"
                    );
                    bad = true;
                }
                if kinds.iter().any(|k| k == "turn_adopt_refused") {
                    bad = true;
                    turn = "fenced".to_string();
                } else if kinds.iter().any(|k| k == "turn_adopted") {
                    turn = "kept".to_string();
                }
            }
        }
        // A stale turn ignored with --ignore-stale can be swept to
        // `unknown` by the replacement store's recovery without any
        // adopt attempt — no `turn_adopt_refused` event exists to mark
        // it. Check each carried row directly so a swept stale turn
        // still fails the restart verdict.
        for (sal, mid) in stale_turns.iter().filter(|(sal, _)| sal == &alias) {
            // A carried stale turn the show cannot verify leaves the
            // verdict unproven — fail closed (CAD-694).
            match client::rpc(state_dir, "agent_show", json!({"alias": sal})) {
                Ok(show) => {
                    let swept = show["messages"]
                        .as_array()
                        .map(|ms| {
                            ms.iter().any(|m| {
                                m["id"].as_str() == Some(mid.as_str())
                                    && m["state"].as_str() == Some("unknown")
                            })
                        })
                        .unwrap_or(false);
                    if swept {
                        bad = true;
                        turn = "fenced".to_string();
                    }
                }
                Err(e) => {
                    eprintln!("restart: could not verify carried stale turn {sal}/{mid}: {e}");
                    bad = true;
                }
            }
        }
        if after_state == "attention" {
            if before_state == "attention" {
                existing_fences.push(alias.clone());
            } else {
                bad = true;
            }
        }
        if live_before.contains(&alias) && matches!(after_state.as_str(), "starting" | "offline") {
            bad = true;
        }
        rows.push((alias, before_state, after_state, pane, turn));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let w = rows.iter().map(|r| r.0.len()).max().unwrap_or(5).max(5);
    println!(
        "{:<w$}  {:<12}  {:<12}  {:<18}  TURN",
        "AGENT",
        "BEFORE",
        "AFTER",
        "PANE",
        w = w
    );
    for (alias, before_state, after_state, pane, turn) in &rows {
        println!(
            "{alias:<w$}  {before_state:<12}  {after_state:<12}  {pane:<18}  {turn}",
            w = w
        );
    }
    if !existing_fences.is_empty() {
        existing_fences.sort();
        println!("Existing fences retained: {}", existing_fences.join(", "));
    }
    if bad {
        Err(Error::rejected(
            "restart completed but not cleanly — see the table above \
             (pane pid changed, a turn was fenced, an agent became \
             fenced, a previously live agent did not settle, or the \
             drain evidence was unverifiable)",
        ))
    } else {
        Ok(0)
    }
}

/// The `last-recovery.json` shape the verdict trusts (CAD-694): every
/// field the writer emits is required and typed, so a truncated,
/// stale-format or hand-edited record is a parse failure — never clean
/// evidence. `consumed` stays a raw `Value` because its members are
/// required-but-nullable (serde `Option` accepts a MISSING key, which
/// must fail instead).
#[derive(serde::Deserialize)]
struct RecoveryRecordView {
    instance: String,
    consumed: serde_json::Value,
    fenced: Vec<FencedRowView>,
    /// Sibling of `fenced` — required for shape completeness; the
    /// verdict keys on `fenced` alone.
    #[allow(dead_code)]
    unevidenced: Vec<FencedRowView>,
}

#[derive(serde::Deserialize)]
struct FencedRowView {
    alias: String,
    /// Required for shape completeness; the report lists aliases only.
    #[allow(dead_code)]
    message_id: String,
}

/// The instance id of the daemon a `daemon start` answer spawned.
/// Only the `started` branch pid-matches its health answer to the
/// child it spawned; `already_running` carries whichever daemon held
/// the socket, so it binds nothing and the verdict fails closed.
fn started_instance(started: &Value) -> Option<String> {
    if started["state"].as_str() != Some("started") {
        return None;
    }
    started["health"]["instance"].as_str().map(str::to_string)
}

/// CAD-694: evaluate the successor daemon's recovery record — drain
/// evidence the verdict needs independent of the per-alias event
/// cursors (a predecessor already dead yields none, and non-pty
/// endpoints were never cursor-covered). `started_id` is the instance
/// the restart's own start returned (pid-matched health), NOT a file:
/// a later boot or a pair of failed writes can leave an old, mutually
/// matching (instance, record) pair on disk, so equality between the
/// two files proves nothing — only naming THIS successor counts.
/// `Some(reason)` fails the verdict: the record is absent, unreadable,
/// wrongly shaped, or bound to a different run; the predecessor's drain
/// failed; or this recovery fenced any in-flight turn.
fn recovery_record_verdict(state_dir: &Path, started_id: Option<&str>) -> Option<String> {
    let Some(started_id) = started_id else {
        return Some(
            "the daemon start answer did not name its instance — the predecessor's \
             drain cannot be verified"
                .to_string(),
        );
    };
    let Some(current) =
        std::fs::read_to_string(state_dir.join(cadence_agent::daemon::INSTANCE_FILE))
            .ok()
            .map(|s| s.trim().to_string())
    else {
        return Some(
            "the new daemon left no readable instance file — the predecessor's \
             drain cannot be verified"
                .to_string(),
        );
    };
    let Some(record) =
        std::fs::read_to_string(state_dir.join(cadence_agent::daemon::LAST_RECOVERY_FILE))
            .ok()
            .and_then(|raw| serde_json::from_str::<RecoveryRecordView>(&raw).ok())
    else {
        return Some(
            "the new daemon left no readable recovery record (missing or malformed) — \
             the predecessor's drain cannot be verified"
                .to_string(),
        );
    };
    if current != started_id {
        return Some(
            "the recorded daemon instance does not name the run this restart \
             started — the predecessor's drain cannot be verified"
                .to_string(),
        );
    }
    if record.instance != started_id {
        return Some(
            "the recovery record names a different run than the one this restart \
             started — the predecessor's drain cannot be verified"
                .to_string(),
        );
    }
    // `consumed` is null or an object whose `failed` member is present
    // and null|string — anything else is evidence that did not come
    // from this build's writer.
    let failed = match &record.consumed {
        Value::Null => Ok(None),
        Value::Object(map) => match (
            map.get("instance").is_some_and(|v| v.is_string()),
            map.get("stale")
                .is_some_and(|v| v.is_string() || v.is_null()),
            map.get("failed"),
        ) {
            (true, true, Some(f)) if f.is_string() || f.is_null() => {
                Ok(f.as_str().map(str::to_string))
            }
            _ => Err("recovery record's `consumed` is malformed"),
        },
        _ => Err("recovery record's `consumed` is malformed"),
    };
    match failed {
        Err(why) => Some(format!("{why} — the drain cannot be verified")),
        Ok(Some(failed)) => Some(format!("the previous daemon's drain failed: {failed}")),
        Ok(None) => {
            if record.fenced.is_empty() {
                None
            } else {
                let names = record
                    .fenced
                    .iter()
                    .map(|f| f.alias.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                Some(format!(
                    "recovery fenced {} in-flight turn(s): {names}",
                    record.fenced.len()
                ))
            }
        }
    }
}

/// `cadence inbox <alias> --follow --exec <cmd>` (CAD-480): push each
/// queued message to the command as JSON on stdin; it is acked
/// (`agent_inbox_ack` watermark) only after the command exits 0. A
/// non-zero exit leaves the message queued and retries it with bounded
/// backoff — `retry_base_ms` doubling per attempt, capped at 30s —
/// head-of-line, so a later message is never acked past a failed one.
/// A run past `timeout_ms` (0 = no limit) is killed and counted as a
/// failure; after `max_failures` consecutive failures on one message
/// the follower parks it (`inbox_park` — it stays queued and unread
/// but the reader's peeks skip it) and moves on. Every failure lands
/// on stderr and in the inbox's `inbox_exec_fail` events. The loop
/// ends only on interrupt or a daemon error.
pub(crate) fn run_inbox_exec(
    state_dir: &Path,
    alias: &str,
    reader: &str,
    argv: &[String],
    retry_base_ms: u64,
    timeout_ms: u64,
    max_failures: u32,
) -> Result<i32> {
    const RETRY_CAP_MS: u64 = 30_000;
    let mut pending: VecDeque<Value> = VecDeque::new();
    let mut attempts: HashMap<i64, u32> = HashMap::new();
    let mut retry_at: Option<Instant> = None;
    loop {
        if let Some(m) = pending.front().cloned() {
            if let Some(t) = retry_at {
                let now = Instant::now();
                if now < t {
                    // Bounded backoff — poll again soon so new arrivals
                    // still queue up behind the failed head.
                    std::thread::sleep((t - now).min(Duration::from_secs(1)));
                    continue;
                }
                retry_at = None;
            }
            let seq = m["seq"].as_i64().unwrap_or_default();
            match inbox_exec_once(argv, &m, timeout_ms) {
                Ok(()) => {
                    client::rpc(
                        state_dir,
                        "agent_inbox_ack",
                        json!({"alias": alias, "through": seq, "reader": reader}),
                    )?;
                    println!("{}", serde_json::to_string(&m).unwrap_or_default());
                    pending.pop_front();
                    attempts.remove(&seq);
                }
                Err(why) => {
                    let a = {
                        let a = attempts.entry(seq).or_insert(0);
                        *a += 1;
                        *a
                    };
                    client::rpc(
                        state_dir,
                        "agent_inbox_ack",
                        json!({"alias": alias, "reader": reader,
                               "fail": {"message": m["id"], "seq": seq,
                                        "attempt": a, "error": why}}),
                    )?;
                    if a >= max_failures {
                        // Poison: park it for this reader — queued and
                        // unread still, but no longer head-of-line.
                        let reason = format!("exec failed {a} times; last: {why}");
                        client::rpc(
                            state_dir,
                            "agent_inbox_ack",
                            json!({"alias": alias, "reader": reader,
                                   "park": m["id"], "reason": reason}),
                        )?;
                        eprintln!(
                            "inbox {alias}: seq {seq} parked after {a} failures \
                             ({why}) — still queued; moving on"
                        );
                        pending.pop_front();
                        attempts.remove(&seq);
                        continue;
                    }
                    let delay = retry_base_ms
                        .saturating_mul(1u64 << a.saturating_sub(1).min(10))
                        .min(RETRY_CAP_MS);
                    eprintln!(
                        "inbox {alias}: exec failed for seq {seq} (attempt {a}): \
                         {why} — the message stays queued; retrying in {delay}ms"
                    );
                    retry_at = Some(Instant::now() + Duration::from_millis(delay));
                }
            }
            continue;
        }
        // Queue empty — long-poll for arrivals. Peek changes nothing:
        // an un-acked message comes back every round until exec
        // succeeds.
        let page = client::rpc(
            state_dir,
            "agent_inbox",
            json!({"alias": alias, "peek": true, "reader": reader, "wait": 25}),
        )?;
        for m in page["messages"].as_array().cloned().unwrap_or_default() {
            let seq = m["seq"].as_i64().unwrap_or_default();
            if !pending.iter().any(|p| p["seq"].as_i64() == Some(seq)) {
                pending.push_back(m);
            }
        }
    }
}

/// Run one `--exec` argv on one message: the message JSON on stdin, no
/// shell. `Ok(())` only on exit 0; `Err` carries the exit status and a
/// stderr tail for the failure report. A run still alive at
/// `timeout_ms` (0 = no limit) is killed — the error names the timeout.
pub(crate) fn inbox_exec_once(
    argv: &[String],
    message: &Value,
    timeout_ms: u64,
) -> std::result::Result<(), String> {
    let mut child = cadence_agent::reaper::spawn(
        Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::piped()),
    )
    .map_err(|e| format!("could not spawn '{}': {e}", argv[0]))?;
    // The write runs on its own thread: a consumer that never reads
    // stdin or exits early must not block the wait on a full pipe.
    let writer = child.stdin.take().map(|mut stdin| {
        let body = serde_json::to_string(message)
            .unwrap_or_default()
            .into_bytes();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&body);
        })
    });
    // Poll try_wait: std has no wait-with-timeout, and a wedged
    // consumer must not block the inbox head-of-line forever.
    let deadline = (timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(timeout_ms));
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {
                if let Some(t) = deadline {
                    if Instant::now() >= t {
                        let _ = child.kill();
                        break true;
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("could not wait on '{}': {e}", argv[0])),
        }
    };
    // Reaped or killed: wait_with_output drains the piped stderr (EOF
    // on exit) and reports the status.
    let out = child
        .wait_with_output()
        .map_err(|e| format!("could not wait on '{}': {e}", argv[0]))?;
    if let Some(w) = writer {
        let _ = w.join();
    }
    if timed_out {
        return Err(format!(
            "'{}' killed after {}ms timeout",
            argv[0], timeout_ms
        ));
    }
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stderr = stderr.trim();
    let tail = if stderr.len() > 500 {
        &stderr[stderr.len() - 500..]
    } else {
        stderr
    };
    Err(format!(
        "'{}' {}: {}",
        argv[0],
        out.status
            .code()
            .map_or_else(|| "died on a signal".to_string(), |c| format!("exited {c}")),
        if tail.is_empty() { "no stderr" } else { tail }
    ))
}

/// `cadence audit approve|revoke` — the operator's approval-evidence
/// writers (CAD-217). The daemon decides authority from the connection;
/// nothing here names the caller.
pub(crate) fn run_audit_evidence(state_dir: &Path, action: AuditAction) -> Result<i32> {
    let result = match action {
        AuditAction::Approve {
            issue: Some(issue),
            source,
            action,
            ..
        } => {
            if action != "scope" {
                return Err(Error::rejected("--issue takes --action scope"));
            }
            client::rpc(
                state_dir,
                "approval_scope",
                json!({"issue": issue, "source": source}),
            )?
        }
        AuditAction::Approve {
            pr,
            head,
            source,
            repo,
            action,
            id,
            delegated,
            scope,
            ..
        } => {
            let pr = pr.ok_or_else(|| Error::rejected("--pr is required"))?;
            let head = head.unwrap_or_default().trim().to_ascii_lowercase();
            let repo = match repo {
                Some(r) => r,
                None => cadence_agent::audit::origin_slug(&std::env::current_dir()?).ok_or_else(
                    || {
                        Error::rejected(
                            "no github.com origin remote in the cwd — pass --repo owner/name",
                        )
                    },
                )?,
            };
            // No `--id`: the daemon picks the default, counting past a
            // revoked one so a re-approval is a fresh record.
            let mut params = json!({"source": source, "action": action,
                                    "head": head, "repo": repo, "pr": pr});
            if let Some(id) = id {
                params["id"] = json!(id);
            }
            if delegated && action != "merge" {
                return Err(Error::rejected("--delegated approves a merge only"));
            }
            if let Some(scope) = scope {
                params["scope"] = json!(scope); // clap: only with --delegated
            }
            let verb = if delegated {
                "approval_delegate"
            } else {
                "approval_record"
            };
            client::rpc(state_dir, verb, params)?
        }
        // Handled in `cli::audit::run`: a read, not an RPC.
        // Handled in `cli::audit::run`: reads, not RPCs.
        AuditAction::Approval { .. } | AuditAction::Verdicts { .. } => {
            unreachable!("audit approval/verdicts are read-only and handled in cli::audit::run")
        }
        AuditAction::Designate {
            alias,
            project,
            source,
            revoke,
        } => client::rpc(
            state_dir,
            "approval_designate",
            json!({"alias": alias, "project": project, "source": source, "active": !revoke}),
        )?,
        AuditAction::Designations => client::rpc(state_dir, "approval_designations", json!({}))?,
        AuditAction::Digest { since } => cadence_agent::audit::digest(state_dir, since.as_deref())?,
        AuditAction::Revoke { id, source, reason } => client::rpc(
            state_dir,
            "approval_revoke",
            json!({"id": id, "source": source, "reason": reason}),
        )?,
    };
    print_json(&result);
    Ok(0)
}

/// Age formatting — `42s`, `13m`, `5h`, `2d`.
pub(crate) fn fmt_age(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

/// `$HOME` — skill install root and the agent-CLI skill dirs all live
/// directly under it (`.agents` has no XDG equivalent).
pub(crate) fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| Error::rejected("HOME is not set to an absolute path"))
}

/// The group root a `CADENCE_ALIAS` caller scopes its view to inside a
/// cadence pane: the caller's `params.upstream` when set, else its own
/// alias. `None` outside a pane, for `--all`, for an unresolvable alias —
/// and for the master (CAD-576): the install's one overseer has no group
/// of its own, so a caller-group scope would show it a fleet of one.
fn caller_group(state_dir: &Path) -> Option<String> {
    let name = std::env::var("CADENCE_ALIAS").ok()?;
    if cadence_agent::master::is_master(&name) {
        return None;
    }
    let caller = client::rpc(state_dir, "agent_show", json!({"alias": name})).ok()?;
    caller["agent"]["params"]["upstream"]
        .as_str()
        .or_else(|| caller["agent"]["alias"].as_str())
        .map(str::to_string)
}

/// `agent list`: global view, or — inside a cadence pane — scoped to the
/// caller's group. The caller's group root is its `params.upstream` when
/// set, else its own alias; scoped output keeps the root plus agents
/// whose upstream names it (groups are one level deep), and marks the
/// root row `"group_root": true`. `CADENCE_ALIAS` unset or unresolvable
/// falls back to the global list untouched. `states`/`providers`/`kinds`
/// filter daemon-side; `projects` filters client-side on the cwd→project
/// mapping (the daemon never opens the PM dir). Filters narrow the scope,
/// never widen it. The applied scope is stamped as `"scope"`: `"all"`,
/// or `{"group": <root>}`.
pub(crate) fn list_agents(
    state_dir: &Path,
    states: &[String],
    providers: &[String],
    kinds: &[String],
    projects: &[String],
    all: bool,
    stamp_project: bool,
) -> Result<Value> {
    let mut list = client::rpc(
        state_dir,
        "agent_list",
        json!({"states": states, "providers": providers, "kinds": kinds}),
    )?;
    // Group decoration on every row: "group" names the root (upstream
    // when wired, else the row's own alias) and roots carry
    // "group_root": true — consumers can render the tree without
    // re-deriving the wiring.
    if let Some(agents) = list["agents"].as_array_mut() {
        for a in agents.iter_mut() {
            let root = group_root_of(a).to_string();
            if a["alias"].as_str() == Some(root.as_str()) {
                a["group_root"] = json!(true);
            }
            a["group"] = json!(root);
        }
    }
    let caller_root = if all { None } else { caller_group(state_dir) };
    if let Some(root) = &caller_root {
        if let Some(agents) = list["agents"].as_array_mut() {
            agents.retain(|a| {
                a["alias"].as_str() == Some(root.as_str())
                    || a["params"]["upstream"].as_str() == Some(root.as_str())
            });
        }
    }
    // CAD-576: which agents the rows cover — "all", or the one group
    // root a pane caller is scoped to. A reader never guesses a
    // count's boundary from an absent field.
    list["scope"] = match caller_root {
        Some(root) => json!({"group": root}),
        None => json!("all"),
    };
    if !projects.is_empty() || stamp_project {
        stamp_agent_projects(&mut list)?;
    }
    if !projects.is_empty() {
        let pm = cadence_agent::issue::Pm::open_default()?;
        let known: Vec<String> = cadence_agent::issue::project::list(&pm.dir)
            .unwrap_or_default()
            .iter()
            .map(|p| p.key.clone())
            .collect();
        for p in projects {
            if !known.iter().any(|k| k == p) {
                return Err(Error::rejected(format!(
                    "Unknown --project '{p}' — known: {}",
                    known.join(" ")
                )));
            }
        }
        if let Some(agents) = list["agents"].as_array_mut() {
            agents.retain(|a| {
                a["project"]
                    .as_str()
                    .is_some_and(|p| projects.iter().any(|want| want == p))
            });
        }
    }
    // The daemon counts before client-side group and project filters.
    // Keep the advertised capacity in the same scope as the visible rows.
    list["live"] = json!(count_live_agents(&list));
    Ok(list)
}

fn count_live_agents(list: &Value) -> usize {
    list["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| {
            a["dead"].as_bool() != Some(true)
                && !matches!(
                    a["state"].as_str().unwrap_or_default(),
                    "stopping" | "stopped" | "attention" | "offline"
                )
        })
        .count()
}

/// Stamp each agent row's `"project"` with the tracker project its cwd
/// resolves to (null when it resolves to none). Needs the PM dir — which
/// `agent list` does not otherwise require — so it opens only when a
/// filter or sort reads the key.
pub(crate) fn stamp_agent_projects(list: &mut Value) -> Result<()> {
    use cadence_agent::issue::{self, project};
    let pm = issue::Pm::open_default()?;
    if let Some(agents) = list["agents"].as_array_mut() {
        for a in agents.iter_mut() {
            let cwd = a["cwd"].as_str().unwrap_or_default();
            let key = project::key_for_cwd(&pm.dir, Path::new(cwd));
            a["project"] = json!(key);
        }
    }
    Ok(())
}

/// `cadence agent resume`: provider-launch treatment for a reopen —
/// bounded wait for the endpoint, then attach this terminal by default.
/// `--detach` or a non-TTY/nested-tmux context prints the attach command
/// instead. Kinds with nothing attachable (managed stdio, fake) return
/// the resume receipt immediately — there is no endpoint to wait for.
pub(crate) fn resume_agent(state_dir: &Path, alias: &str, detach: bool) -> Result<i32> {
    let result = client::rpc(state_dir, "agent_resume", json!({"alias": alias}))?;
    let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
    let agent = &show["agent"];
    let (provider, kind) = (
        agent["provider"].as_str().unwrap_or_default(),
        agent["endpoint_kind"].as_str().unwrap_or_default(),
    );
    if !registry::attachable(provider, kind) {
        // Non-attachable actors prove their open by reaching a live
        // state — finish_resume waits for that (bounded) before any
        // briefing/AGENTS.md housekeeping.
        print_json(&finish_resume(state_dir, alias, result));
        return Ok(0);
    }
    // Poll until the endpoint is live or the actor gives up.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (agent, unknown) = loop {
        let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
        let agent = show["agent"].clone();
        let state = agent["state"].as_str().unwrap_or_default();
        if agent["endpoint"].is_string() || matches!(state, "stopped" | "offline" | "attention") {
            break (agent, show["unknown"].as_i64().unwrap_or(0));
        }
        if Instant::now() >= deadline {
            return Err(Error::rejected(format!(
                "Agent '{alias}' opened no endpoint within 30s — inspect \
                 `cadence agent show {alias}` and attach when it is live"
            )));
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let state = agent["state"].as_str().unwrap_or_default();
    // A fenced agent (attention, no endpoint) gets the recovery hint —
    // unreconciled unknowns reconcile first — anything else attaches.
    let next = if state == "attention" && agent["endpoint"].is_null() {
        fenced_next(alias, agent["error"].as_str().unwrap_or_default(), unknown)
    } else {
        json!({"attach": format!("cadence agent attach {alias}")})
    };
    print_json(&finish_resume(
        state_dir,
        alias,
        json!({
            "alias": alias, "state": state,
            "endpoint": agent["endpoint"], "next": next,
        }),
    ));
    if detach || agent["endpoint"].is_null() {
        return Ok(0);
    }
    if atty_stdin() && std::env::var_os("TMUX").is_none() {
        return attach_agent(state_dir, alias, true);
    }
    attach_agent(state_dir, alias, false)
}

pub(crate) fn print_json(value: &Value) {
    println!(
        "{}",
        cadence_agent::output::json_text(value).unwrap_or_default()
    );
}

/// Sort/limit/fields on `out[key]` — the shared list-grammar tail for
/// daemon-RPC payloads, which already arrive as JSON. `names` is the
/// documented sort-key set (`flag name`, `row key`); without `--sort`
/// the wire order stands.
pub(crate) fn shape_rows(
    out: &mut Value,
    key: &str,
    sort: Option<&str>,
    names: &[(&str, &str)],
    tie: &str,
    limit: Option<usize>,
    fields: &[String],
) -> Result<()> {
    let Some(rows) = out.get_mut(key).and_then(Value::as_array_mut) else {
        return Ok(());
    };
    if let Some(spec) = sort {
        cadence_agent::filter::sort_rows(rows, spec, names, tie)?;
    }
    cadence_agent::filter::apply_limit(rows, limit);
    cadence_agent::filter::apply_fields(rows, fields)?;
    Ok(())
}

/// An agent row's group root: its `params.upstream` when wired, else its
/// own alias (a standalone agent is trivially its own group).
pub(crate) fn group_root_of(agent: &Value) -> &str {
    agent["params"]["upstream"]
        .as_str()
        .or_else(|| agent["alias"].as_str())
        .unwrap_or_default()
}

/// Aliases of a group's members — every agent whose upstream names the
/// root (one level; groups are not transitive today).
pub(crate) fn group_members(state_dir: &Path, root: &str) -> Result<Vec<String>> {
    let list = client::rpc(state_dir, "agent_list", json!({}))?;
    Ok(list["agents"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|a| a["params"]["upstream"].as_str() == Some(root))
        .filter_map(|a| a["alias"].as_str().map(str::to_string))
        .collect())
}

/// The two provider errors that mean "the pane bound a different native
/// session" — resume can never converge on these, so they get the
/// remove-and-rejoin hint.
pub(crate) fn session_mismatch(error: &str) -> bool {
    error.contains("owns session") || error.contains("acquired session")
}

/// `next` hint for a fenced agent (`attention`, no endpoint). An
/// unreconciled `unknown` is an inspection problem: the hint does not
/// hand out an unfence-then-resume command chain. A session-mismatch
/// can never converge — remove and rejoin (each retried resume mints a
/// fresh provider session); anything else retries `agent resume`.
pub(crate) fn fenced_next(alias: &str, error: &str, unknown: i64) -> Value {
    if session_mismatch(error) {
        json!({
            "remove": format!("cadence agent remove {alias}"),
            "rejoin": "cadence join <pm> <provider> -r <session>",
            "note": "retrying resume mints a new provider session each time",
        })
    } else if unknown > 0 {
        json!({
            "inspect": cadence_agent::daemon::unknown_inspect_lead(),
            "decision": cadence_agent::daemon::unknown_recovery_note(),
        })
    } else {
        json!({"resume": format!("cadence agent resume {alias}")})
    }
}

/// Resume one registered agent, waiting — bounded — for the endpoint
/// when the kind has one. Live agents are skipped; terminal-state or
/// RPC failures land in the per-member `error`. The unrecoverable case
/// (the pane adopted a different native session) gets an explicit
/// remove-and-rejoin hint.
pub(crate) fn resume_one(state_dir: &Path, alias: &str) -> Value {
    let (agent, unknown) = match client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
        Ok(show) => (show["agent"].clone(), show["unknown"].as_i64().unwrap_or(0)),
        Err(e) => return json!({"alias": alias, "resumed": false, "error": e.to_string()}),
    };
    // Live = a live actor (idle/running/waiting_input/starting) or a
    // live endpoint address — fake/managed actors never expose one, so
    // endpoint alone cannot detect "already up".
    // A mailbox has nothing to resume — its queue survives regardless.
    let (provider, kind) = (
        agent["provider"].as_str().unwrap_or_default(),
        agent["endpoint_kind"].as_str().unwrap_or_default(),
    );
    if !registry::has_actor(provider, kind) {
        return json!({"alias": alias, "resumed": false, "skipped": "mailbox"});
    }
    let live = agent["endpoint"].is_string()
        || matches!(
            agent["state"].as_str(),
            Some("idle") | Some("running") | Some("waiting_input") | Some("starting")
        );
    if live {
        return finish_resume(
            state_dir,
            alias,
            json!({"alias": alias, "resumed": false, "skipped": "live"}),
        );
    }
    // Fenced by an unreconciled `unknown` — never attempted; the sweep
    // reports it under `fenced` with the reconcile-first commands.
    if unknown > 0 {
        return json!({"alias": alias, "resumed": false, "fenced": true,
        "state": agent["state"],
        "hint": format!(
            "fenced by an unreconciled unknown message — not resumed. {} {}",
            cadence_agent::daemon::unknown_inspect_lead(),
            cadence_agent::daemon::unknown_recovery_note()
        )});
    }
    // Any other `attention` fence gates resume exactly the way it gates
    // startup relaunch — the recorded cause is the operator's context;
    // a session-mismatch can never converge (each retried resume mints
    // a new provider session), anything else may retry once cleared.
    if agent["state"].as_str() == Some("attention") {
        let error = agent["error"].as_str().unwrap_or_default().to_string();
        let hint = if session_mismatch(&error) {
            format!(
                "unrecoverable — `cadence agent remove {alias}` then rejoin with \
                     `cadence join <pm> <provider> -r <session>`; each retried resume \
                     mints a new provider session"
            )
        } else {
            format!(
                "fenced (attention) — `cadence agent show {alias}` records the \
                     cause; retry `cadence agent resume {alias}` once it is cleared"
            )
        };
        return json!({"alias": alias, "resumed": false, "fenced": true,
                      "state": "attention", "error": error, "hint": hint});
    }
    let attachable = registry::attachable(provider, kind);
    if let Err(e) = client::rpc(state_dir, "agent_resume", json!({"alias": alias})) {
        return json!({"alias": alias, "resumed": false, "error": e.to_string()});
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let agent = client::rpc(state_dir, "agent_show", json!({"alias": alias}))
            .map(|s| s["agent"].clone())
            .unwrap_or_default();
        let state = agent["state"].as_str().unwrap_or_default();
        if agent["endpoint"].is_string() {
            return finish_resume(
                state_dir,
                alias,
                json!({"alias": alias, "resumed": true,
                          "endpoint": agent["endpoint"]}),
            );
        }
        // Kinds without an attachable endpoint are done once the actor
        // is back — there is nothing to wait on.
        if !attachable && matches!(state, "idle" | "running") {
            return finish_resume(state_dir, alias, json!({"alias": alias, "resumed": true}));
        }
        if matches!(state, "attention" | "stopped" | "offline") {
            let error = agent["error"].as_str().unwrap_or("unknown").to_string();
            let mut out = json!({"alias": alias, "resumed": false,
                                 "state": state, "error": error});
            if session_mismatch(&error) {
                // The pane bound a different native session — resume
                // can never converge; rebuild the member instead.
                out["hint"] = json!(format!(
                    "unrecoverable — `cadence agent remove {alias}` then \
                     rejoin with `cadence join <pm> <provider> -r <session>`; \
                     retrying resume mints a new provider session each time"
                ));
                out["unrecoverable"] = json!(true);
            }
            return out;
        }
        if Instant::now() >= deadline {
            return json!({"alias": alias, "resumed": false,
                          "error": "endpoint did not open within 15s"});
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Post-resume housekeeping on a successfully resumed agent: regenerate
/// a missing briefing in its state-dir location (agents registered
/// before briefings moved out of the repo have none there) and re-apply
/// the opt-in AGENTS.md block — the persisted `agents_md` param replays
/// here like the other launch params. Best-effort: the resume already
/// succeeded, so a failure surfaces as a warning field, not an error.
pub(crate) fn finish_resume(state_dir: &Path, alias: &str, mut out: Value) -> Value {
    match refresh_briefing(state_dir, alias) {
        Ok(Some(file)) => out["briefing"] = json!(file),
        Ok(None) => {}
        Err(e) => out["briefing_warning"] = json!(e.to_string()),
    }
    out
}

/// `Some(path)` when a briefing was (re)written, `None` when nothing
/// was needed. Post-open only, same rule as launch: the actor must
/// prove it is live before anything is written — a resume whose open
/// stalls or fences writes nothing (terminal states exit early, a slow
/// open gives up after 15s).
pub(crate) fn refresh_briefing(state_dir: &Path, alias: &str) -> Result<Option<PathBuf>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let agent = loop {
        let agent = client::rpc(state_dir, "agent_show", json!({"alias": alias}))
            .map(|s| s["agent"].clone())
            .unwrap_or_default();
        let state = agent["state"].as_str().unwrap_or_default();
        if agent["endpoint"].is_string() || matches!(state, "idle" | "running" | "waiting_input") {
            break agent;
        }
        if matches!(state, "stopped" | "offline" | "attention") || Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    if !registry::has_actor(
        agent["provider"].as_str().unwrap_or_default(),
        agent["endpoint_kind"].as_str().unwrap_or_default(),
    ) {
        return Ok(None);
    }
    let file = client::briefing_path(state_dir, &agent["params"], alias);
    let opted_in = agent["params"]["agents_md"].as_bool() == Some(true);
    let split_pty = agent["endpoint_kind"].as_str() == Some("pty")
        && cadence_agent::agent_uid::config::configured_uid(state_dir)?.is_some();
    if file.exists() && !opted_in && !split_pty {
        return Ok(None);
    }
    brief_agent(state_dir, alias, false, None).map(Some)
}

/// Resume a list of aliases in order, printing a per-member status line
/// and returning the `{resumed, skipped, fenced, failed}` summary.
/// Fenced members are never attempted — they land in `fenced` with the
/// reconcile-first hint.
pub(crate) fn resume_sweep(state_dir: &Path, aliases: &[String]) -> Value {
    let (mut resumed, mut skipped, mut fenced, mut failed) = (vec![], vec![], vec![], vec![]);
    for alias in aliases {
        let r = resume_one(state_dir, alias);
        if r["resumed"].as_bool() == Some(true) {
            eprintln!("resume {alias}: up");
            resumed.push(r);
        } else if r["skipped"].is_string() {
            eprintln!(
                "resume {alias}: skipped ({})",
                r["skipped"].as_str().unwrap_or("")
            );
            skipped.push(r);
        } else if r["fenced"].as_bool() == Some(true) {
            eprintln!(
                "resume {alias}: FENCED — {}",
                r["hint"]
                    .as_str()
                    .unwrap_or("reconcile its unknown messages")
            );
            fenced.push(r);
        } else {
            let reason = r["error"].as_str().unwrap_or("unknown");
            eprintln!("resume {alias}: FAILED — {reason}");
            failed.push(r);
        }
    }
    json!({"resumed": resumed, "skipped": skipped,
           "fenced": fenced, "failed": failed})
}

/// `cadence resume <group>` / `cadence resume --all`.
pub(crate) fn resume_command(
    state_dir: &Path,
    group: Option<String>,
    all: bool,
    detach: bool,
) -> Result<i32> {
    if all {
        let targets = client::rpc(state_dir, "agent_list", json!({}))?["agents"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter(|a| {
                a["endpoint"].is_null()
                    && (a["thread_id"].is_string() || a["session_id"].is_string())
            })
            .filter_map(|a| a["alias"].as_str().map(str::to_string))
            .collect::<Vec<_>>();
        print_json(&resume_sweep(state_dir, &targets));
        return Ok(0);
    }
    let group = group.expect("clap requires group unless --all");
    // Resolve like join: alias or provider-native id → the PM agent.
    let pm = client::rpc(state_dir, "agent_show", json!({"alias": group})).map_err(|_| {
        Error::rejected(format!(
            "Unknown group '{group}' — no such agent; \
                 `cadence agent list` shows registered aliases"
        ))
    })?["agent"]
        .clone();
    let pm_alias = pm["alias"].as_str().unwrap_or_default().to_string();
    // PM first, then members — only those without a live endpoint
    // (resume_one re-checks and reports the live ones as skipped).
    let mut order = vec![pm_alias.clone()];
    order.extend(group_members(state_dir, &pm_alias)?);
    print_json(&resume_sweep(state_dir, &order));
    if detach {
        return Ok(0);
    }
    // Attach to the PM pane by default — same rules as provider_launch:
    // exec only where this terminal can.
    let pm = client::rpc(state_dir, "agent_show", json!({"alias": pm_alias}))?["agent"].clone();
    if pm["endpoint"].is_null() {
        return Ok(0);
    }
    if atty_stdin() && std::env::var_os("TMUX").is_none() {
        return attach_agent(state_dir, &pm_alias, true);
    }
    attach_agent(state_dir, &pm_alias, false)
}

/// `cadence stop <group>`: members first, then the PM — agents stay
/// registered and resumable. Per-member outcomes are reported, never
/// silently dropped.
pub(crate) fn stop_group(state_dir: &Path, group: &str) -> Result<i32> {
    let pm = client::rpc(state_dir, "agent_show", json!({"alias": group})).map_err(|_| {
        Error::rejected(format!(
            "Unknown group '{group}' — no such agent; \
                 `cadence agent list` shows registered aliases"
        ))
    })?["agent"]
        .clone();
    let pm_alias = pm["alias"].as_str().unwrap_or_default().to_string();
    let mut order = group_members(state_dir, &pm_alias)?;
    order.push(pm_alias);
    let mut stopped = vec![];
    let mut skipped = vec![];
    let mut failed = vec![];
    for alias in &order {
        // A mailbox is never "stopped" — it has no actor and its queue
        // is the point. Removing it is the only lifecycle action.
        let is_inbox = client::rpc(state_dir, "agent_show", json!({"alias": alias}))
            .map(|s| {
                !registry::has_actor(
                    s["agent"]["provider"].as_str().unwrap_or_default(),
                    s["agent"]["endpoint_kind"].as_str().unwrap_or_default(),
                )
            })
            .unwrap_or(false);
        if is_inbox {
            eprintln!("stop {alias}: skipped (inbox — durable mailbox)");
            skipped.push(json!({"alias": alias}));
            continue;
        }
        match client::rpc(state_dir, "agent_stop", json!({"alias": alias})) {
            Ok(r) => {
                eprintln!("stop {alias}: {}", r["state"].as_str().unwrap_or("ok"));
                stopped.push(json!({"alias": alias, "state": r["state"]}));
            }
            Err(e) => {
                eprintln!("stop {alias}: FAILED — {e}");
                failed.push(json!({"alias": alias, "error": e.to_string()}));
            }
        }
    }
    print_json(&json!({"stopped": stopped, "skipped": skipped, "failed": failed}));
    Ok(0)
}

/// Email-style flags callers guess for `message send` / `send`.
pub(crate) const EMAIL_FLAGS: [&str; 4] = ["--to", "--subject", "--body", "--cc"];

/// `message send --to …` would get clap's `to pass '--to' as a value,
/// use '-- --to'` tip, which steers the caller to smuggle the flag in as
/// text. Swap that for the real usage line; every other parse error
/// (help and version included) passes through untouched.
pub(crate) fn email_flag_error(err: &clap::Error) -> Option<clap::Error> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    if err.kind() != ErrorKind::UnknownArgument {
        return None;
    }
    let Some(ContextValue::String(arg)) = err.get(ContextKind::InvalidArg) else {
        return None;
    };
    let flag = arg.split('=').next().unwrap_or_default();
    if !EMAIL_FLAGS.contains(&flag) {
        return None;
    }
    // The usage line names the subcommand the parse failed in.
    let Some(ContextValue::StyledStr(usage)) = err.get(ContextKind::Usage) else {
        return None;
    };
    let usage = usage.to_string();
    let verb = if usage.contains(" message send ") {
        "message send"
    } else if usage.contains(" send ") {
        "send"
    } else {
        return None;
    };
    // CAD-888: top-level `send` accepts `--to` as the recipient.
    let (banned, recipient) = if verb == "send" {
        (
            "--subject/--body/--cc",
            "the positional alias or --to <alias>",
        )
    } else {
        ("--to/--subject/--body/--cc", "the positional alias")
    };
    Some(clap::Error::raw(
        ErrorKind::UnknownArgument,
        format!(
            "`cadence {verb}` has no `{flag}` flag — it takes no email-style \
             {banned}; the recipient is {recipient}\n\n\
             Usage: cadence {verb} <ALIAS> --text <body>\n\n\
             \x20 body: --text <body>, -m <body> or --file <path>\n\
             \x20 multi-topic report: open the body with `SUBJECT: <topic>`\n\n\
             For more information, try 'cadence {verb} --help'.\n"
        ),
    ))
}

/// When this process is the master, an exact argv the operator
/// approved is run by the daemon. Returns `Some` when that happened
/// (or the daemon refused a never-list command). `None` means the
/// normal verb should run.
fn permission_replay(state_dir: &Path, cli: &Cli) -> Option<i32> {
    if std::env::var("CADENCE_ALIAS").ok().as_deref() != Some(cadence_agent::master::ALIAS) {
        return None;
    }
    if std::env::var_os("CADENCE_GRANT_TOKEN").is_some() {
        return None;
    }
    if let Commands::Master {
        action:
            master::MasterAction::AskPermission { .. }
            | master::MasterAction::PeekGrant { .. }
            | master::MasterAction::UseGrant { .. },
    } = &cli.command
    {
        return None;
    }
    // The daemon launches the master's provider as `cadence confine`.
    // That wrapper inherits CADENCE_ALIAS=master. It is not a tool
    // command, and there is no daemon to ask yet when the provider
    // itself is what is starting.
    if let Commands::Confine { .. } = &cli.command {
        return None;
    }
    // The tracker's pre-commit hook runs `cadence issue lint` with the
    // committer's environment. A master commit inherits
    // CADENCE_ALIAS=master. Sending that through a permission RPC
    // deadlocks the commit (the daemon is inside it) or refuses it
    // when the socket is a different daemon. Lint is read-only and
    // must finish for every tracker write, including verdicts.
    if let Commands::Issue {
        action: cadence_agent::issue::cli::IssueAction::Lint { .. },
    } = &cli.command
    {
        return None;
    }
    // `report` and `report file` file done reports, verdicts and ideas.
    // They are allowlisted for the master, or the local handler refuses
    // stdin (`NO_STDIN`). A grant lookup must not sit in front of them:
    // the process the daemon is waiting on is this one, and a permission
    // RPC from inside that wait never returns.
    if let Commands::Report { .. } = &cli.command {
        return None;
    }
    let mut argv = vec!["cadence".to_string()];
    argv.extend(std::env::args().skip(1));
    let cwd = std::env::current_dir().ok()?;
    match cadence_agent::master_perm::classify(&argv, &cwd, &[], std::slice::from_ref(&state_dir)) {
        cadence_agent::master_perm::Class::Allowlisted => return None,
        cadence_agent::master_perm::Class::Never { why } => {
            eprintln!("{why} — a grant or a rule cannot allow it");
            return Some(1);
        }
        _ => {}
    }
    match client::rpc(
        state_dir,
        "master_permission_use",
        json!({"argv": argv, "cwd": cwd}),
    ) {
        Ok(out) if out["applied"].as_bool() == Some(true) => {
            let stdout = out["stdout"].as_str().unwrap_or("");
            let stderr = out["stderr"].as_str().unwrap_or("");
            if !stdout.is_empty() {
                print!("{stdout}");
                if !stdout.ends_with('\n') {
                    println!();
                }
            }
            if !stderr.is_empty() {
                eprint!("{stderr}");
                if !stderr.ends_with('\n') {
                    eprintln!();
                }
            }
            Some(out["code"].as_i64().unwrap_or(1) as i32)
        }
        Ok(_) => None,
        Err(e) => {
            eprintln!("{e}");
            Some(1)
        }
    }
}

/// The `agent_show` request `cadence self` sends (CAD-879): running
/// turns only, no message history.
fn self_show_params(alias: &str) -> Value {
    json!({"alias": alias, "active_only": true})
}

/// Where a release lives, which repository is trusted, and the operator
/// identity — shared by `cadence update` and `cadence update status` so
/// the flags parse on either side of the subcommand.
#[derive(clap::Args, Default)]
pub(crate) struct UpdateTargetArgs {
    /// Operator identity outside a cadence pane, for example
    /// `operator:ada`. Required outside a pane; inside one this
    /// command is refused.
    #[arg(long = "as")]
    as_identity: Option<String>,
    /// GitHub repository whose CI built and attested the binary.
    #[arg(long, default_value = cadence_agent::upgrade::DEFAULT_REPO)]
    repo: String,
    /// Symlink that puts cadence on PATH [default: ~/.local/bin/cadence].
    #[arg(long)]
    link: Option<PathBuf>,
    /// Releases directory [default: read off the current link].
    #[arg(long)]
    releases_dir: Option<PathBuf>,
}

impl UpdateTargetArgs {
    /// The subcommand's flags when it carried any, else the parent's.
    fn or(self, parent: UpdateTargetArgs) -> UpdateTargetArgs {
        UpdateTargetArgs {
            as_identity: self.as_identity.or(parent.as_identity),
            repo: if self.repo.is_empty() {
                parent.repo
            } else {
                self.repo
            },
            link: self.link.or(parent.link),
            releases_dir: self.releases_dir.or(parent.releases_dir),
        }
    }
}

pub(crate) struct UpgradeArgs {
    sha: Option<String>,
    latest_main: bool,
    dry_run: bool,
    restart: bool,
    as_identity: Option<String>,
    allow_unattested: bool,
    repo: String,
    link: Option<PathBuf>,
    releases_dir: Option<PathBuf>,
}

pub(crate) struct UpdateArgs {
    status: bool,
    check: bool,
    rollback: bool,
    drain: String,
    now: bool,
    keep: u64,
    backup_dir: Option<PathBuf>,
    json: bool,
    progress: Option<PathBuf>,
    as_identity: Option<String>,
    repo: String,
    link: Option<PathBuf>,
    releases_dir: Option<PathBuf>,
}

/// The production [`cadence_agent::update::UpdateHost`]: the daemon for
/// the drain, the waiters and the health answers; the filesystem for
/// the releases and the marker.
pub(crate) struct RealUpdateHost<'a> {
    state_dir: &'a Path,
    layout: cadence_agent::upgrade::Layout,
    source: cadence_agent::upgrade::Gh,
    label: String,
    /// Collect the plain lines for `--json`; `None` prints them as they
    /// happen (the plain form).
    collect: Option<std::cell::RefCell<Vec<String>>>,
    /// The marker this run last recorded, for the drain re-assertion.
    pending: std::cell::RefCell<Option<cadence_agent::update::PendingUpdate>>,
    /// `--progress`: append every line here too, so the board's card can
    /// read the run while this process is detached from it (CAD-561 r2).
    progress_log: Option<PathBuf>,
}

impl RealUpdateHost<'_> {
    fn line(&self, line: &str) {
        let log = self.progress_log.as_deref();
        if let Some(path) = log {
            cadence_agent::update::run_log_line(path, line);
        }
        match &self.collect {
            Some(lines) => lines.borrow_mut().push(line.to_string()),
            // The board starts the helper with stdout pointing at the same
            // progress log; printing there too would write every line
            // twice (CAD-561 r3).
            None if !log.is_some_and(|path| fd_is_path(libc::STDOUT_FILENO, path)) => {
                println!("{line}")
            }
            None => {}
        }
    }
}

/// Is descriptor `fd` the same file as `path`? The helper's stdout is
/// the progress log when the board starts it, so its progress lines must
/// not also be printed there (CAD-561 r3).
pub(crate) fn fd_is_path(fd: i32, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(descriptor), Ok(file)) = (
        std::fs::metadata(format!("/proc/self/fd/{fd}")),
        std::fs::metadata(path),
    ) else {
        return false;
    };
    descriptor.dev() == file.dev() && descriptor.ino() == file.ino()
}

impl cadence_agent::update::UpdateHost for RealUpdateHost<'_> {
    fn state_dir(&self) -> &Path {
        self.state_dir
    }
    fn layout(&self) -> &cadence_agent::upgrade::Layout {
        &self.layout
    }
    fn source(&self) -> &dyn cadence_agent::upgrade::ReleaseSource {
        &self.source
    }
    fn identity(&self) -> &str {
        &self.label
    }
    fn progress(&self, line: &str) {
        self.line(line);
    }
    fn waiters(&self) -> Result<Vec<cadence_agent::update::Waiter>> {
        // The daemon's own view is authoritative (it reads the message
        // rows); with no daemon running nothing is in flight.
        let status = match client::rpc(self.state_dir, "update_status", json!({})) {
            Ok(status) => status,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(status["waiting"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| {
                        Some(cadence_agent::update::Waiter {
                            alias: r["alias"].as_str()?.to_string(),
                            message: r["message"].as_str().unwrap_or_default().to_string(),
                            state: r["state"].as_str().unwrap_or_default().to_string(),
                            age_secs: r["age_secs"].as_u64().unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
    fn set_pending(&self, pending: Option<&cadence_agent::update::PendingUpdate>) -> Result<()> {
        *self.pending.borrow_mut() = pending.cloned();
        match pending {
            Some(pending) => cadence_agent::update::write_pending(self.state_dir, pending),
            None => {
                cadence_agent::update::clear_pending(self.state_dir);
                Ok(())
            }
        }
    }
    fn pending(&self) -> Option<cadence_agent::update::PendingUpdate> {
        self.pending.borrow().clone()
    }
    fn set_drain(&self, on: bool) -> Result<()> {
        let mut params = json!({"on": on, "label": self.label});
        if on {
            // The phase comes from the marker the pipeline just wrote;
            // a daemon that is not running is not an error (there is
            // nothing to drain, and the restart will start it).
            let pending = self.pending.borrow().clone();
            if let Some(pending) = pending {
                params["target"] = json!(pending.target);
                params["phase"] = json!(pending.phase);
                params["from"] = json!(pending.from);
                params["since"] = json!(pending.since);
            }
        }
        match client::rpc(self.state_dir, "update_drain", params) {
            Ok(_) => Ok(()),
            // A daemon that is down mid-update (the switch) has nothing
            // to gate; the marker on disk covers the restart.
            Err(e) if e.to_string().starts_with("Daemon is not reachable") => Ok(()),
            Err(e) => Err(e),
        }
    }
    fn restart(&self, binary: &Path) -> Result<cadence_agent::update::RestartOutcome> {
        use cadence_agent::update::RestartOutcome;
        // Rollback may select an older CLI whose `daemon restart`
        // requires a live fleet RPC. Select its cold-start command
        // before invoking it, with the current updater's guards.
        let caller = update::update_caller(self.state_dir, Some(&self.label))?;
        refuse_restart_over_leftovers(self.state_dir)?;
        let ticket = cadence_agent::rollout::begin_restart(self.state_dir, &caller)?;
        let offline = match client::rpc_answer(self.state_dir, "agent_list", json!({})) {
            Ok(Ok(_)) => false,
            Ok(Err(refused)) => return Err(refused),
            Err(_) => {
                if !daemon_lock_free(self.state_dir) {
                    return Err(Error::rejected(
                        "daemon owns the state-dir lock but its fleet snapshot is unavailable — \
                         refusing updater recovery before invoking the release binary",
                    ));
                }
                true
            }
        };
        let mut commands: Vec<Vec<String>> = if offline {
            vec![vec![
                "daemon".into(),
                "start".into(),
                "--as".into(),
                self.label.clone(),
            ]]
        } else {
            vec![vec![
                "daemon".into(),
                "restart".into(),
                "--ui".into(),
                "--as".into(),
                self.label.clone(),
            ]]
        };
        if let Some(ui_pid) = offline
            .then(|| cadence_agent::ui::detached_pid(self.state_dir))
            .flatten()
        {
            // Preserve the restart's --ui contract when a board survived
            // the failed replacement. Read legacy argv before stopping
            // the process; persisted ui.json remains authoritative.
            let mut ui_start = vec!["ui".into(), "start".into()];
            if !cadence_agent::ui::opts_present(self.state_dir) {
                let (host, port, dist, allow_hosts) = ui_run_args(ui_pid);
                ui_start.extend(["--host".into(), host, "--port".into(), port.to_string()]);
                if let Some(dist) = dist {
                    ui_start.extend(["--dist".into(), dist.to_string_lossy().into_owned()]);
                }
                for host in allow_hosts {
                    ui_start.extend(["--allow-host".into(), host]);
                }
            }
            commands.extend([vec!["ui".into(), "stop".into()], ui_start]);
        }
        for args in commands {
            cadence_agent::rollout::recheck_restart(self.state_dir, &ticket)?;
            refuse_restart_over_leftovers(self.state_dir)?;
            if offline && args[0] == "daemon" {
                // Never shut down a daemon that appeared after the
                // offline proof. Its own singleton is the final start
                // protection; this route never restores a backup or
                // bypasses the selected daemon's schema checks.
                if !daemon_lock_free(self.state_dir) {
                    return Err(Error::rejected(
                        "daemon acquired the state-dir lock before updater recovery — retry",
                    ));
                }
                cadence_agent::rollout::note_restart_proceeded(self.state_dir, &ticket)?;
            }
            let mut cmd = Command::new(binary);
            cmd.arg("--state-dir")
                .arg(self.state_dir)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            if args[0] == "ui" && args[1] == "start" {
                cmd.env_remove("CADENCE_ALIAS");
            }
            let out = cadence_agent::reaper::spawn(&mut cmd)
                .and_then(|child| child.wait_with_output())
                .map_err(|e| Error::internal(format!("could not run {}: {e}", binary.display())))?;
            if out.status.success() {
                continue;
            }
            // A non-zero exit is the restart's complaint, not a stop
            // (CAD-561 r3): a fenced turn makes `daemon restart` exit
            // non-zero with the new build up, and a failed `daemon start`
            // or `ui start` leaves the daemon or the board down — only the
            // health check that follows can tell, and it rolls back.
            let complaint = String::from_utf8_lossy(&out.stderr);
            let complaint = complaint.trim();
            let exit = out.status.code().unwrap_or(-1);
            return Ok(RestartOutcome::Unclean(if complaint.is_empty() {
                format!("the restart on {} exited {exit}", binary.display())
            } else {
                format!(
                    "the restart on {} exited {exit}: {complaint}",
                    binary.display()
                )
            }));
        }
        Ok(RestartOutcome::Clean)
    }
    fn daemon_build(&self) -> Result<Option<String>> {
        // CAD-598 r4/N2: a health poll must not sit on the default
        // 700s rpc timeout — a daemon that is slow to answer would
        // hold `health_wait` well past HEALTH_TIMEOUT on a single
        // call. A few seconds is the poll's whole budget; the wait
        // retries whatever the short window misses (N1).
        daemon_build_or_absent(client::rpc_timeout(
            self.state_dir,
            "daemon_info",
            json!({}),
            DAEMON_BUILD_TIMEOUT,
        ))
    }
    fn board_build(&self) -> Result<Option<String>> {
        match cadence_agent::ui::health(self.state_dir) {
            None => Ok(None),
            Some((_port, body)) => Ok(serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["build"].as_str().map(str::to_string))),
        }
    }
    fn board_running(&self) -> bool {
        cadence_agent::ui::detached_pid(self.state_dir).is_some()
    }
    fn progress_log(&self) -> Option<PathBuf> {
        self.progress_log.clone()
    }
    fn now(&self) -> f64 {
        cadence_agent::rollout::unix_now()
    }
    fn sleep(&self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }
}

/// The health poll's per-call bound (CAD-598 r4/N2): a daemon that is
/// slow to answer gets this long per probe, never the default 700s —
/// `health_wait`'s deadline is the real bound.
pub(crate) const DAEMON_BUILD_TIMEOUT: Duration = Duration::from_secs(5);

/// `daemon_build`'s answer classification (CAD-561 r4): only an
/// unreachable daemon reads as "no daemon" — every other RPC failure
/// is real and must fail the run, or `finish_restart` restarts a
/// daemon that was merely slow to answer.
pub(crate) fn daemon_build_or_absent(answer: Result<Value>) -> Result<Option<String>> {
    match answer {
        Ok(info) => Ok(info["build_commit"].as_str().map(str::to_string)),
        Err(e) if e.to_string().starts_with("Daemon is not reachable") => Ok(None),
        Err(e) => Err(e),
    }
}

/// Claude-specific launch options — stored under `params` so the
/// adapter replays them verbatim on every resume (`--permission-mode`,
/// `--allowedTools`, `--model`).
#[derive(Default)]
pub(crate) struct ClaudeOpts {
    model: Option<String>,
    /// `params.effort` — the CLI's `--effort` level.
    effort: Option<String>,
    permission_mode: Option<String>,
    allow: Vec<String>,
    bypass: bool,
    /// `params.broker_approvals` — route permission prompts to
    /// `agent requests`/`agent respond` via the mcp-permission server.
    broker_approvals: bool,
    /// `params.permission_timeout_secs` — operator-decision deadline
    /// before a brokered prompt denies (default 900).
    permission_timeout_secs: Option<u64>,
    /// `params.turn_idle_secs` — inactivity window before a turn is
    /// `unknown` (activity-based liveness; default 900).
    turn_idle_secs: Option<u64>,
    /// `params.turn_max_secs` — optional absolute turn cap.
    turn_max_secs: Option<u64>,
    /// `params.confine` (CAD-556) — pi only: `Some(true)` from
    /// `--confine`, `Some(false)` from `--no-confine`, `None` defers
    /// to the pm.yaml `[host] confine_pi_workers` default.
    confine: Option<bool>,
}

/// Devin-specific launch options. Pty stores `permission_mode` so the
/// same argv replays on every pane open. `--cloud` stores the v3
/// create params instead; `--bypass` is the pty `dangerous` shorthand
/// and is refused for a cloud session.
#[derive(Default)]
pub(crate) struct DevinOpts {
    permission_mode: Option<String>,
    bypass: bool,
    cloud: bool,
    repos: Vec<String>,
    devin_mode: Option<String>,
    max_acu_limit: Option<u64>,
    playbook_id: Option<String>,
    knowledge_ids: Vec<String>,
    secret_ids: Vec<String>,
    platform: Option<String>,
    tags: Vec<String>,
    bypass_approval: bool,
    attachment_urls: Vec<String>,
}

/// Cursor-specific launch options — `params.model` and
/// `params.permission_mode` ride the profile so the same
/// `--model`/`--force`/`--auto-review` argv replays on every pane open.
/// `--bypass` is the `force` shorthand.
#[derive(Default)]
pub(crate) struct CursorOpts {
    model: Option<String>,
    permission_mode: Option<String>,
    bypass: bool,
}

/// Codex app-server launch settings. They are stored in the agent params so
/// the adapter can replay the same model and reasoning effort on resume.
#[derive(Default)]
pub(crate) struct CodexOpts {
    model: Option<String>,
    effort: Option<String>,
    /// `params.approval_policy`, replayed on every open.
    approval_policy: Option<String>,
    /// `params.turn_idle_secs` / `params.turn_max_secs` — the same
    /// activity-based turn liveness as managed claude (CAD-227).
    turn_idle_secs: Option<u64>,
    turn_max_secs: Option<u64>,
}

pub(crate) fn launch_endpoint_kind(provider: &str, cloud: bool, tui: bool) -> Result<&'static str> {
    if cloud {
        if provider != "devin" {
            return Err(Error::rejected(
                "--cloud is only supported for provider devin",
            ));
        }
        if tui {
            return Err(Error::rejected("--cloud cannot be combined with --tui"));
        }
        return Ok("cloud");
    }
    if tui {
        if registry::spec_opt(provider, "pty").is_some() {
            return Ok("pty");
        }
        return Err(Error::rejected(format!(
            "provider '{provider}' has no pty endpoint — `--tui` is only \
             meaningful for claude (devin is already a TUI)"
        )));
    }
    registry::default_kind(provider)
}

/// `cadence devin [-r slug]` / `cadence codex` / `cadence claude`:
/// register the provider's endpoint, wait for it to open, then attach
/// this terminal by default where the kind has an attachable surface.
/// Re-running against an already-registered name resumes or reuses it.
/// Once open, the agent answers to its alias and its provider-native id
/// alike (e.g. `cadence agent show <devin-session-slug>`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn provider_launch(
    state_dir: &Path,
    provider: &str,
    cwd: Option<PathBuf>,
    role: &str,
    alias: Option<String>,
    resume: Option<String>,
    instructions_file: Option<PathBuf>,
    detach: bool,
    upstream: Option<String>,
    worktree: Option<&str>,
    briefing: BriefMode,
    auto_ready: bool,
    agents_md: bool,
    tui: bool,
    // `--sandbox`; `None` resolves per provider below.
    sandbox: Option<String>,
    claude: &ClaudeOpts,
    devin: &DevinOpts,
    cursor: &CursorOpts,
    codex: &CodexOpts,
    team_role: Option<&str>,
    provider_default_model: bool,
) -> Result<i32> {
    // Role instructions reach every provider but codex only through the
    // briefing — refuse before anything is registered, created or
    // launched rather than store text nothing will ever read.
    if instructions_file.is_some() && briefing == BriefMode::Off && provider != "codex" {
        return Err(Error::rejected(format!(
            "--instructions-file with --no-bootstrap is refused for provider \
             '{provider}': the instructions would have no delivery channel — \
             only codex takes them natively; every other provider receives \
             them in the briefing, which --no-bootstrap skips. Drop \
             --no-bootstrap to deliver them in the briefing"
        )));
    }
    // A codex-only setting on another provider is refused, not dropped.
    if provider != "codex" && codex.approval_policy.is_some() {
        return Err(Error::rejected("--approval-policy only applies to codex"));
    }
    // `--tui` selects the provider's pty endpoint where one exists;
    // otherwise the launch kind comes from the registry's default.
    let endpoint_kind = launch_endpoint_kind(provider, devin.cloud, tui)?;
    // `-r` on claude only makes sense on the pty endpoint — the managed
    // adapter reopens through `agent resume` and would silently drop a
    // session param it never reads. Pi has no native-session seed to
    // point `-r` at either: a managed pi worker resumes the session
    // file the adapter keeps under the state dir, via `agent resume`.
    if provider == "claude" && resume.is_some() && !tui {
        return Err(Error::rejected(
            "`--resume` on claude requires `--tui` — a managed claude agent \
             resumes with `cadence agent resume <alias>`",
        ));
    }
    if provider == "pi" && resume.is_some() {
        return Err(Error::rejected(
            "`--resume` on pi is not supported — a managed pi worker resumes \
             its stored session file with `cadence agent resume <alias>`",
        ));
    }
    // A pi worker has no permission surface to flag (CAD-544): its
    // toolset is the fixed dev allowlist, unattended — refuse the
    // claude-style knobs rather than silently drop them.
    if provider == "pi" {
        if claude.permission_mode.is_some() || claude.bypass {
            return Err(Error::rejected(
                "--permission-mode/--bypass do not apply to pi — a managed pi \
                 worker runs with its fixed tool allowlist, unattended",
            ));
        }
        if !claude.allow.is_empty() {
            return Err(Error::rejected("--allow only applies to claude"));
        }
        if claude.broker_approvals {
            return Err(Error::rejected("--broker-approvals only applies to claude"));
        }
    }
    // `-r <slug>` first resolves the slug to an already-registered agent
    // (by alias or native session id) so re-running is a reopen, not a
    // duplicate registration fighting over the same session lock.
    let mut alias = alias.clone();
    if alias.is_none() {
        if let Some(name) = &resume {
            if let Some(show) = lookup_agent(state_dir, name)? {
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
    // An alias keeps its provider and endpoint kind — refuse before any
    // worktree, register or resume rather than reopen the old agent under
    // the new verb or flags. The lookup fails closed: only the daemon's
    // not-found answer means "not registered" (CAD-305).
    let existing = lookup_agent(state_dir, &alias)?;
    if let Some(show) = &existing {
        if show["agent"]["alias"].as_str() == Some(alias.as_str()) {
            refuse_endpoint_mismatch(&alias, &show["agent"], provider, endpoint_kind)?;
        }
    }
    let cwd = match cwd {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    // `--worktree` creates an isolated checkout under the repo's
    // `.cadence/wt/` — refuse before creating anything when the
    // resolved agent already exists: a reopen keeps its stored cwd.
    if worktree.is_some() && existing.is_some() {
        return Err(Error::rejected(format!(
            "'{alias}' is already registered — --worktree only applies to a \
             new agent; reuse the existing checkout via --cwd"
        )));
    }
    let cwd = match worktree {
        Some(name) => create_worktree(&cwd, name)?,
        None => cwd,
    };
    if auto_ready && !registry::screen_probe(provider, endpoint_kind) {
        return Err(Error::rejected(
            "--auto-ready only applies to pty endpoints — a screen probe \
             exists only there",
        ));
    }
    let instructions = instructions_file.map(std::fs::read_to_string).transpose()?;
    let mut params_obj = serde_json::Map::new();
    if let Some(session) = &resume {
        params_obj.insert("session".to_string(), Value::String(session.clone()));
    }
    if let Some(upstream) = &upstream {
        params_obj.insert("upstream".to_string(), Value::String(upstream.clone()));
    }
    // Claude's launch params ride in `params` so the adapter replays
    // them verbatim on resume; the turn-liveness keys stay gated on the
    // endpoint spec below — managed-only, since pty liveness is the
    // pane itself, not provider event activity.
    if provider == "claude" {
        let spec = registry::spec(provider, endpoint_kind)?;
        if let Some(model) = &claude.model {
            params_obj.insert("model".to_string(), json!(model));
        }
        if let Some(effort) = &claude.effort {
            params_obj.insert("effort".to_string(), json!(effort));
        }
        let permission_mode = if claude.bypass {
            Some("bypassPermissions".to_string())
        } else {
            claude.permission_mode.clone()
        };
        if let Some(mode) = permission_mode {
            params_obj.insert("permission_mode".to_string(), json!(mode));
        }
        if !claude.allow.is_empty() {
            params_obj.insert("allowed_tools".to_string(), json!(claude.allow));
        }
        // `--broker-approvals` is managed-only: the verbs refuse it
        // with `--tui`/`--bypass` already, and the spec gate keeps any
        // non-verb path honest the same way `turn_idle_secs` is gated.
        if claude.broker_approvals {
            if tui || claude.bypass {
                return Err(Error::rejected(
                    "--broker-approvals is refused with --tui and --bypass — \
                     a pane answers its own prompts and bypass makes them moot",
                ));
            }
            if spec.launch_params.contains(&"broker_approvals") {
                params_obj.insert("broker_approvals".to_string(), json!(true));
            }
            if let Some(secs) = claude.permission_timeout_secs {
                params_obj.insert("permission_timeout_secs".to_string(), json!(secs));
            }
        }
        if spec.launch_params.contains(&"turn_idle_secs") {
            if let Some(secs) = claude.turn_idle_secs {
                params_obj.insert("turn_idle_secs".to_string(), json!(secs));
            }
        }
        if spec.launch_params.contains(&"turn_max_secs") {
            if let Some(secs) = claude.turn_max_secs {
                params_obj.insert("turn_max_secs".to_string(), json!(secs));
            }
        }
    }
    // Devin's `permission_mode` persists the same way — the profile
    // replays it into the pane argv on every open, so a `--bypass`
    // worker never stalls on its first approval menu again.
    if provider == "devin" && endpoint_kind == "pty" {
        let mode = if devin.bypass {
            Some("dangerous")
        } else {
            devin.permission_mode.as_deref()
        };
        if let Some(mode) = mode {
            registry::devin_permission_mode(mode)?;
            params_obj.insert("permission_mode".to_string(), json!(mode));
        }
    }
    if endpoint_kind == "cloud" {
        insert_devin_cloud_params(&mut params_obj, devin)?;
    }
    // Cursor's model/permission params persist the same way — the
    // profile replays them into the pane argv on every open; `--bypass`
    // stores `force`.
    if provider == "cursor" {
        if let Some(model) = &cursor.model {
            params_obj.insert("model".to_string(), json!(model));
        }
        let mode = if cursor.bypass {
            Some("force")
        } else {
            cursor.permission_mode.as_deref()
        };
        if let Some(mode) = mode {
            registry::cursor_permission_mode(mode)?;
            params_obj.insert("permission_mode".to_string(), json!(mode));
        }
    }
    if provider == "codex" {
        if let Some(model) = &codex.model {
            params_obj.insert("model".to_string(), json!(model));
        }
        if let Some(effort) = &codex.effort {
            registry::codex_effort(effort)?;
            params_obj.insert("effort".to_string(), json!(effort));
        }
        if let Some(policy) = &codex.approval_policy {
            registry::codex_approval_policy(policy)?;
            params_obj.insert("approval_policy".to_string(), json!(policy));
        }
        if let Some(secs) = codex.turn_idle_secs {
            params_obj.insert("turn_idle_secs".to_string(), json!(secs));
        }
        if let Some(secs) = codex.turn_max_secs {
            params_obj.insert("turn_max_secs".to_string(), json!(secs));
        }
    }
    // Pi's launch params ride in `params` exactly like claude's — the
    // adapter replays them verbatim on every open: `--model`, then
    // `effort` through `set_thinking_level` (verified via `get_state`).
    if provider == "pi" {
        let spec = registry::spec(provider, endpoint_kind)?;
        if let Some(model) = &claude.model {
            params_obj.insert("model".to_string(), json!(model));
        }
        if let Some(effort) = &claude.effort {
            registry::pi_effort(effort)?;
            params_obj.insert("effort".to_string(), json!(effort));
        }
        if spec.launch_params.contains(&"turn_idle_secs") {
            if let Some(secs) = claude.turn_idle_secs {
                params_obj.insert("turn_idle_secs".to_string(), json!(secs));
            }
        }
        if spec.launch_params.contains(&"turn_max_secs") {
            if let Some(secs) = claude.turn_max_secs {
                params_obj.insert("turn_max_secs".to_string(), json!(secs));
            }
        }
        // CAD-556 — the flag is tri-state: `--confine`/`--no-confine`
        // always win over the pm.yaml `[host] confine_pi_workers`
        // default the daemon applies when the key is absent entirely.
        // `--confine` refuses up front on a host without Landlock —
        // an opt-in that silently degraded to unconfined would be the
        // worst kind of surprise.
        if let Some(confine) = claude.confine {
            if confine {
                cadence_agent::confine::available()
                    .map_err(|e| Error::rejected(format!("--confine: {e}")))?;
            }
            params_obj.insert("confine".to_string(), json!(confine));
        }
    } else if claude.confine.is_some() {
        return Err(Error::rejected(
            "--confine/--no-confine only apply to provider `pi` — Landlock \
             confinement is the pi worker's today",
        ));
    }
    if auto_ready {
        params_obj.insert(
            "auto_ready".to_string(),
            Value::String("verified".to_string()),
        );
    }
    // `--agents-md` is a cadence-level opt-in, not provider config —
    // it rides in params so resume replays it like the other launch
    // params.
    if agents_md {
        params_obj.insert("agents_md".to_string(), Value::Bool(true));
    }
    if provider_default_model {
        if params_obj.contains_key("model") {
            return Err(Error::invalid(
                "conflicting_model_policy",
                "an explicit model cannot be combined with --provider-default-model",
            ));
        }
        if !registry::supports_model(provider, endpoint_kind) {
            return Err(Error::invalid(
                "unsupported_model_setting",
                format!("provider '{provider}' endpoint '{endpoint_kind}' does not accept a model"),
            ));
        }
    }
    if endpoint_kind == "cloud" {
        registry::validate_launch_params(
            provider,
            endpoint_kind,
            &Value::Object(params_obj.clone()),
        )?;
    }
    let params = (!params_obj.is_empty()).then(|| Value::Object(params_obj).to_string());
    // The sandbox rides the agent record; codex sends it on
    // `thread/start`. A cadence-launched codex worker is writable by
    // default — the same trust posture the other providers already run
    // — and `read-only` remains available when explicitly asked.
    let sandbox = sandbox.unwrap_or_else(|| {
        if provider == "codex" {
            "workspace-write"
        } else {
            "read-only"
        }
        .to_string()
    });
    // Reopening an already-registered name keeps its stored params — a
    // requested upstream is not retro-applied to a pre-existing agent.
    let mut registered_fresh = false;
    match client::rpc(
        state_dir,
        "agent_register",
        json!({"alias": alias, "provider": provider,
        "endpoint_kind": endpoint_kind, "cwd": cwd,
        "role": role, "sandbox": sandbox,
        "instructions": instructions, "params": params,
        "team_role": team_role,
        "model_policy": if provider_default_model {
            Some("provider_default")
        } else {
            None::<&str>
        }}),
    ) {
        Ok(_) => registered_fresh = true,
        Err(err) if err.to_string().contains("UNIQUE") => {
            // Already registered — reopen rather than fail. A stopped
            // agent is resumed; a live one is reused as-is.
            let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
            refuse_endpoint_mismatch(&alias, &show["agent"], provider, endpoint_kind)?;
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
    // The provider endpoint opens asynchronously (a pty open can wait on
    // the native session lock) — poll until it is live or gives up.
    // Kinds with no attachable endpoint are done once the actor is back.
    let attachable = registry::attachable(provider, endpoint_kind);
    let deadline = Instant::now() + Duration::from_secs(45);
    let (agent, unknown) = loop {
        let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
        let agent = show["agent"].clone();
        let state = agent["state"].as_str().unwrap_or_default();
        let open = agent["endpoint"].is_string();
        if open
            || matches!(state, "stopped" | "offline" | "attention")
            || (!attachable && matches!(state, "idle" | "running"))
        {
            break (agent, show["unknown"].as_i64().unwrap_or(0));
        }
        if Instant::now() >= deadline {
            break (agent, show["unknown"].as_i64().unwrap_or(0));
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let state = agent["state"].as_str().unwrap_or_default();
    // A fresh agent gets its briefing (and for joins, or an explicit
    // --bootstrap, the durable kickoff message) only once the endpoint
    // reports open — a launch that fences or stalls leaves the cwd
    // repository byte-identical. Normal gating applies on the message:
    // a pty pane still needs the ready claim.
    let opened = agent["endpoint"].is_string()
        || (!attachable && matches!(state, "idle" | "running" | "waiting_input"));
    let mut briefing_file = Value::Null;
    if opened && briefing != BriefMode::Off {
        if registered_fresh {
            let file = brief_agent(
                state_dir,
                &alias,
                briefing == BriefMode::FilesAndMessage,
                instructions.as_deref(),
            )?;
            briefing_file = json!(file);
        } else if let Ok(Some(file)) = refresh_briefing(state_dir, &alias) {
            // A re-launched pre-existing agent: regen a missing
            // briefing / re-apply an opted-in AGENTS.md — the same
            // post-open housekeeping `agent resume` runs.
            briefing_file = json!(file);
        }
    }
    let native = agent["session_id"]
        .as_str()
        .or_else(|| agent["thread_id"].as_str());
    // A fenced agent (attention, no endpoint) cannot attach — the
    // useful next step is its recovery hint, not the usual trio.
    let fenced = state == "attention" && agent["endpoint"].is_null();
    let next = if fenced {
        fenced_next(&alias, agent["error"].as_str().unwrap_or_default(), unknown)
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
        "permission_mode": agent["params"]["permission_mode"],
        "briefing": briefing_file,
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
pub(crate) fn join_group(
    state_dir: &Path,
    group: &str,
    provider: &str,
    resume: Option<String>,
    tui: bool,
    detach: bool,
    cwd: Option<PathBuf>,
    alias: Option<String>,
    role: &str,
    sandbox: Option<String>,
    instructions_file: Option<PathBuf>,
    worktree: Option<String>,
    no_bootstrap: bool,
    auto_ready: bool,
    agents_md: bool,
    claude_opts: ClaudeOpts,
    devin_opts: DevinOpts,
    cursor_opts: CursorOpts,
    codex_opts: CodexOpts,
    team_role: Option<String>,
    provider_default_model: bool,
) -> Result<i32> {
    // Validate the provider before any work — the registry names the
    // supported launch verbs in the rejection.
    registry::default_kind(provider)?;
    let show = client::rpc(state_dir, "agent_show", json!({"alias": group})).map_err(|_| {
        Error::rejected(format!(
            "Unknown group '{group}' — no such agent; \
                 `cadence agent list` shows registered aliases"
        ))
    })?;
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
    provider_launch(
        state_dir,
        provider,
        cwd,
        role,
        alias,
        resume,
        instructions_file,
        detach,
        Some(pm_alias),
        worktree.as_deref(),
        if no_bootstrap {
            BriefMode::Off
        } else {
            BriefMode::FilesAndMessage
        },
        auto_ready,
        agents_md,
        tui,
        sandbox,
        &claude_opts,
        &devin_opts,
        &cursor_opts,
        &codex_opts,
        team_role.as_deref(),
        provider_default_model,
    )
}

/// The daemon's answer for a name that resolves to no agent, by alias or
/// provider-native id.
pub(crate) const UNKNOWN_AGENT: &str = "Unknown managed agent";

/// `agent_show` for a launch's pre-create checks, failing closed: `None`
/// only for the daemon's not-found answer. Any other error — an
/// unreachable daemon, an ambiguous native id — is returned, never read
/// as "not registered", so nothing is created on a guess (CAD-305).
pub(crate) fn lookup_agent(state_dir: &Path, name: &str) -> Result<Option<Value>> {
    found_or_absent(client::rpc(state_dir, "agent_show", json!({"alias": name})))
}

pub(crate) fn found_or_absent(shown: Result<Value>) -> Result<Option<Value>> {
    match shown {
        Ok(show) => Ok(Some(show)),
        Err(Error::Rejected(message)) if message == UNKNOWN_AGENT => Ok(None),
        Err(err) => Err(err),
    }
}

/// CAD-283 / CAD-305: reopening a registered alias under a different
/// provider, or the same provider on a different endpoint kind (managed
/// vs `--tui`, devin pty vs `--cloud`), would silently resume the old
/// agent and drop what was asked for — refuse, naming both and the
/// remove-then-launch path (there is no in-place swap).
pub(crate) fn refuse_endpoint_mismatch(
    alias: &str,
    agent: &Value,
    provider: &str,
    endpoint_kind: &str,
) -> Result<()> {
    let registered = agent["provider"].as_str().unwrap_or_default();
    if registered != provider {
        return Err(Error::rejected(format!(
            "'{alias}' is already registered as a {registered} agent, not {provider} — \
             an alias keeps its provider. To replace it, run `cadence agent remove \
             {alias}`, then launch or join {provider} under that alias"
        )));
    }
    let registered_kind = agent["endpoint_kind"].as_str().unwrap_or_default();
    if registered_kind == endpoint_kind {
        return Ok(());
    }
    Err(Error::rejected(format!(
        "'{alias}' is already registered as a {provider} {registered_kind} agent, \
         not {provider} {endpoint_kind} — an alias keeps its endpoint kind. To \
         reopen it as registered, run `cadence agent resume {alias}`; to replace \
         it, run `cadence agent remove {alias}`, then launch or join {provider} \
         {endpoint_kind} under that alias"
    )))
}

/// Run a git subcommand in `dir`, returning stdout or a rejected error
/// carrying stderr.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = cadence_agent::reaper::output(Command::new("git").arg("-C").arg(dir).args(args))
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

/// `git worktree add <root>/.cadence/wt/<name> -b cadence/<name>` — the
/// new checkout becomes the agent's cwd. Shared with `issue start`
/// through `cadence_agent::worktree`.
pub(crate) fn create_worktree(base: &Path, name: &str) -> Result<PathBuf> {
    cadence_agent::worktree::create_worktree(base, name)
}

/// `--older-than` duration: bare seconds or an s/m/h/d-suffixed value.
pub(crate) fn parse_duration(text: &str) -> Result<f64> {
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

/// This binary's top-level verbs — setup names a fix only by a verb
/// that exists.
pub(crate) fn cli_verbs() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect()
}
