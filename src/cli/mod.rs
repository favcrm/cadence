// CAD-535: CLI surface — clap definitions, dispatch, shared helpers.
// All code here is moved verbatim from src/main.rs.
// CAD-984: the `Commands` enum lives in `commands.rs`; `run()` and the
// verb dispatch live in `action_dispatch.rs` and are re-exported above.
// CAD-1002: the provider-launch opts cluster lives in `launch.rs`, the
// update/upgrade args and `RealUpdateHost` in `update_args.rs`, the
// resume/stop lifecycle helpers in `resume_helpers.rs`, and the small
// loose helpers (`git`, `create_worktree`, `parse_duration`, `cli_verbs`)
// in `util.rs` — all re-exported below.

pub(crate) use daemon::{
    daemon_lock_free, refuse_restart_over_leftovers, ui_run_args, wait_daemon_exit,
};
mod action_dispatch;
mod agent;
mod agent_uid;
mod app;
mod artifact;
mod attach;
mod attachment;
mod audit;
mod backup;
use cadence_agent::briefing;
mod build_slot;
mod business_templates;
mod claude;
mod client_update;
mod cloud_agent;
mod codex;
mod commands;
mod confine;
mod connection;
mod cursor;
mod daemon;
mod delivery;
mod devin;
mod discovery;
mod dispatch;
mod doctor;
mod events;
mod export;
mod guidance;
mod help;
mod hosted_app;
mod idea;
mod inbox;
mod intake;
mod interrupt;
mod issue;
mod job;
mod join;
mod launch;
mod local_agent;
mod master;
mod mcp_agent;
mod mcp_permission;
mod memory;
mod message;
mod milestone;
mod monitor;
mod org;
mod overview;
mod plan;
mod platform;
mod project;
mod remote;
mod remote_result;
mod report;
mod restore;
mod resume;
mod resume_helpers;
mod review;
mod rollout;
mod sandbox;
mod secret;
mod self_info;
mod send;
mod session;
mod settings;
mod setup;
mod skill;
mod staging;
mod status;
mod stop;
mod test_cmd;
#[cfg(test)]
mod tests;
mod thread;
mod topic;
mod ui;
mod update;
mod update_args;
// CAD-1406: independent refusal-acceptance for the update identity
// and managed-runtime guards; the file is owned by its own author.
#[cfg(all(test, feature = "test-seam"))]
mod update_recovery_tests;
// CAD-1355: independent routing acceptance for `update` ignoring a saved
// remote default — the file is owned by its own author.
mod upgrade;
mod util;
mod web;

// CAD-1479: independently owned backend-default refusal acceptance.
mod wiki;
mod workflow;

pub(crate) use action_dispatch::run;
use agent::AgentAction;
use agent_uid::AgentUidAction;
use app::AppAction;
use artifact::ArtifactAction;
use attach::attach_agent;
use attachment::AttachmentAction;
use audit::AuditAction;
use briefing::{brief_agent, BriefMode};
#[cfg(test)]
use briefing::{cloud_session_prompt, ensure_agents_block, AGENTS_BEGIN, AGENTS_END};
use build_slot::BuildSlotAction;
use business_templates::{DocumentAction, TemplateAction};
use cadence_agent::adapter::registry;
use cadence_agent::client;
use cadence_agent::error::Error;
use cadence_agent::error::Result;
use clap::Parser;
use clap::Subcommand;
use commands::{Commands, PlacementArg};
use connection::ConnectionAction;
use daemon::DaemonAction;
use delivery::DeliveryAction;
pub(crate) use devin::{apply_cloud_params, insert_devin_cloud_params, split_cloud_params};
use idea::IdeaAction;
use inbox::InboxAction;
use intake::IntakeAction;
use job::JobAction;
#[cfg(test)]
use launch::{found_or_absent, launch_endpoint_kind, refuse_endpoint_mismatch, UNKNOWN_AGENT};
use launch::{join_group, provider_launch, ClaudeOpts, CodexOpts, CursorOpts, DevinOpts};
use master::MasterAction;
use message::MessageAction;
use monitor::MonitorAction;
use plan::PlanAction;
use platform::PlatformAction;
use project::ProjectCmd;
use remote_result::RemoteAction;
use report::ReportAction;
#[cfg(test)]
use resume_helpers::session_mismatch;
use resume_helpers::{
    email_flag_error, fenced_next, finish_resume, refresh_briefing, resume_agent, resume_command,
    resume_sweep, stop_group,
};
use rollout::RolloutAction;
use secret::SecretAction;
use serde_json::json;
use serde_json::Value;
use session::SessionAction;
use skill::SkillAction;
use staging::StagingAction;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::time::Instant;
use thread::ThreadAction;
use topic::TopicAction;
use update::UpdateAction;
#[cfg(test)]
use update_args::{daemon_build_or_absent, fd_is_path};
use update_args::{RealUpdateHost, UpdateArgs, UpdateSource, UpdateTargetArgs, UpgradeArgs};
use util::{cli_verbs, create_worktree, parse_duration};
use uuid::Uuid;
use web::WebAction;
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
    /// Select an org for this command — pins the destination once. Conflicts
    /// with an explicit `--state-dir` or a separate tracker pin. A managed
    /// caller (`CADENCE_ALIAS`) may pass only the hosted org of its single
    /// running assignment when the saved login is still that assignment's
    /// original member; registry defaults and org switch stay refused.
    #[arg(long, global = true, value_name = "ORG")]
    org: Option<String>,
    /// Seconds a remote command waits while its org's container wakes
    /// (CAD-1019 slice 2). Only used when the selected org is remote.
    #[arg(long, global = true, default_value_t = cadence_agent::remote_cli::WAKE_TIMEOUT_DEFAULT)]
    wake_timeout: u64,
    /// The argument ids the command line itself typed, read at the CLI
    /// entry (`action_dispatch::run`). `None` when no entry read them, so
    /// the client-only updater refuses instead of assuming none were typed.
    #[arg(skip)]
    command_line: Option<Vec<String>>,
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
    /// Remove the legacy local AgenticOS credential; remote CLI sessions use --org.
    Logout {
        /// Revoke and remove this registered remote workspace's CLI device session.
        #[arg(long)]
        org: Option<String>,
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
    let check = client::rpc_relay(state_dir, "message_report", check_params)?;
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
            client::rpc_relay(
                state_dir,
                "agent_ready",
                json!({"alias": alias, "by": by, "force": force}),
            )?;
        }
    }
    let receipt = client::rpc_relay(
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
        AuditAction::Approval { .. }
        | AuditAction::Verdicts { .. }
        | AuditAction::Scope { .. }
        | AuditAction::Verdict { .. } => {
            unreachable!("audit approval/verdicts/scope/verdict are handled in cli::audit::run")
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
                let wanted = |p: &str| projects.iter().any(|want| want == p);
                a["project"].as_str().is_some_and(wanted)
                    || a["work_projects"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|p| p.as_str())
                        .any(wanted)
            });
        }
    }
    // The daemon counts before client-side group and project filters.
    // Keep the advertised capacity in the same scope as the visible rows.
    list["live"] = json!(count_live_agents(&list));
    Ok(list)
}

/// Keep only rows whose derived placement matches, and recompute the
/// derived `live` count when the payload carries one, so a filtered read
/// never reports capacity for rows it no longer shows.
pub(crate) fn retain_by_placement(view: &mut Value, want: registry::Placement) {
    if let Some(rows) = view["agents"].as_array_mut() {
        rows.retain(|r| {
            registry::placement(
                r["provider"].as_str().unwrap_or_default(),
                r["endpoint_kind"].as_str().unwrap_or_default(),
            ) == want
        });
    }
    if view.get("live").is_some() {
        view["live"] = json!(count_live_agents(view));
    }
    if view.get("footer").is_some() {
        view["footer"]["live"] = json!(count_live_agents(view));
    }
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
    // CAD-1752: the board's rule — projects of the issues an agent is
    // dispatched on, or owns in doing/review — beside the cwd's project.
    let mut owned: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let issues = issue::board::load_all(&pm.dir, None).unwrap_or_default();
    for v in issue::board::views(&pm.config.notes_dir(), issues) {
        if matches!(v.status.as_str(), "doing" | "review") {
            if let Some(owner) = &v.issue.front.owner {
                owned
                    .entry(owner.clone())
                    .or_default()
                    .push(v.issue.project.clone());
            }
        }
    }
    if let Some(agents) = list["agents"].as_array_mut() {
        for a in agents.iter_mut() {
            let cwd = a["cwd"].as_str().unwrap_or_default();
            let key = project::key_for_cwd(&pm.dir, Path::new(cwd));
            let mut work: Vec<String> = a["lane_issues"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|id| id.as_str())
                .filter_map(|id| project::key_for_issue(&pm.dir, id))
                .collect();
            work.extend(
                owned
                    .get(a["alias"].as_str().unwrap_or_default())
                    .cloned()
                    .unwrap_or_default(),
            );
            work.sort();
            work.dedup();
            a["project"] = json!(key);
            a["work_projects"] = json!(work);
        }
    }
    Ok(())
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
    // CAD-1586: `cadence settings …` and `master status`/`master resume`
    // carry their own authorization — the daemon's settings grant gate
    // decides (operator runs free, a designated caller needs a bound
    // settings grant). `master_perm` never-lists these, so the approved-
    // argv replay below could only refuse them, which would shadow the
    // exact-payload grant binding behind a different gate. Nothing else
    // is exempted: every other master verb still goes through replay.
    if let Commands::Settings { .. } = &cli.command {
        return None;
    }
    if let Commands::Master {
        action: master::MasterAction::Status { .. } | master::MasterAction::Resume { .. },
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
