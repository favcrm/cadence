//! CLI for hosted local-agent coordination (CAD-1460).
//!
//! These commands talk to
//! `/__platform/hosted-cadence/:organizationId/local-assignments`.
//! They do not forward `issue start` or `dispatch` into the container.
//! A claim is not execution until the configured local actor accepts the
//! issue note through the existing daemon send.

#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};

use cadence_agent::local_assignment::delivery::deliver_kickoff;
#[cfg(test)]
use cadence_agent::local_assignment::delivery::{
    claimant_target, redact_secrets, require_context, write_note,
};
use cadence_agent::local_assignment::http::BoardHttp;
use cadence_agent::local_assignment::{Coordinator, Kickoff};
use cadence_agent::remote_cli::RemoteTarget;
use clap::Subcommand;
use serde_json::{json, Value};

use super::{print_json, Error, Result};

#[derive(Subcommand)]
pub(crate) enum LocalAgentAction {
    /// List one hosted directory page with server-authored presence and registration state.
    Directory {
        /// Continue from the previous page's after_id; null means exhausted.
        #[arg(long)]
        after_id: Option<String>,
    },
    /// Store an FYI receipt only: never start inference, send work, or copy referenced content.
    Notify {
        /// Stable lag_<UUID> from the selected hosted workspace's directory.
        agent_id: String,
        /// Literal FYI body (1..4000 UTF-16 code units).
        #[arg(long)]
        text: String,
        /// Explicit idempotency key (1..120 units). Retry identical content with this same key.
        #[arg(long)]
        operation_id: String,
        /// Issue id pointer only; repeat up to 16 total references.
        #[arg(long = "issue")]
        issues: Vec<String>,
        /// Wiki id/path pointer only; no file or page is read.
        #[arg(long = "wiki")]
        wiki: Vec<String>,
    },
    /// Save this machine's local actor and claim queued work for you.
    Connect {
        /// Local daemon alias that will read the issue.
        #[arg(long)]
        alias: Option<String>,
        /// Local git checkout. Without it the alias is only saved.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Lane slug. Defaults to `work`.
        #[arg(long)]
        name: Option<String>,
        /// Register this explicit agent for FYI only; preserve the assignment default.
        #[arg(long)]
        fyi_only: bool,
    },
    /// Observe the daemon's durable workspace inbox; never claim work.
    Watch,
    /// Stop local pickup without stopping actors or cancelling their claims.
    Disconnect,
    /// Show this machine's alias and the assignments the board returned.
    Status,
    /// Display durable FYI notices. --clear removes displayed informational notices only.
    Inbox {
        #[arg(long)]
        clear: bool,
    },
    /// Report a claim this machine already holds.
    Report {
        assignment: String,
        /// `running`, `blocked`, `failed`, or `result`.
        #[arg(long)]
        state: String,
        #[arg(long)]
        text: Option<String>,
        /// 40 or 64 lowercase hex characters. Required for result.
        #[arg(long)]
        head: Option<String>,
    },
    /// Move or cancel a live assignment.
    Reassign {
        assignment: String,
        /// Directory user id that should take the work.
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        cancel: bool,
    },
    /// Save one optional legacy tool/publisher credential; does not activate tooling.
    Tool {
        /// Legacy `provider.draft` or `publish.send`; normal account images need neither.
        #[arg(long)]
        lane: String,
    },
}

fn coordinator(target: &RemoteTarget) -> Result<Coordinator<BoardHttp>> {
    BoardHttp::open(target)?.into_coordinator()
}

pub(super) fn run(
    target: &RemoteTarget,
    action: &LocalAgentAction,
    _wake_timeout: u64,
    local_state: Option<PathBuf>,
) -> Result<i32> {
    // These explicit hosted reads/writes need neither a coordinator ledger nor
    // a running daemon. BoardHttp retains saved-login renewal and identity pins.
    match action {
        LocalAgentAction::Directory { after_id } => {
            return finish(&cadence_agent::local_assignment::notifications::directory(
                target,
                after_id.as_deref(),
            )?);
        }
        LocalAgentAction::Notify {
            agent_id,
            operation_id,
            text,
            issues,
            wiki,
        } => {
            let value = cadence_agent::local_assignment::notifications::notify(
                target,
                agent_id,
                operation_id,
                text,
                issues,
                wiki,
            )?;
            print_json(&value);
            return Ok(if value["status"] == "stored" { 0 } else { 5 });
        }
        _ => {}
    }
    let mut coord = coordinator(target)?;
    let value = match action {
        LocalAgentAction::Directory { .. } | LocalAgentAction::Notify { .. } => {
            unreachable!("handled before opening the coordinator")
        }
        LocalAgentAction::Connect {
            repo,
            name,
            alias,
            fyi_only,
        } => {
            let state = local_state
                .clone()
                .map(Ok)
                .unwrap_or_else(cadence_agent::client::state_dir)?;
            ensure_report_driver(&state);
            let alias = alias
                .clone()
                .or(coord.configured_alias()?)
                .ok_or_else(|| Error::usage("connect requires --alias <explicit local agent>"))?;
            cadence_agent::client::rpc_timeout(
                &state,
                "hosted_local_agent_connect",
                json!({
                    "org": target.org, "organization_id": target.org_id, "endpoint": target.endpoint,
                    "alias": alias, "repo": repo, "name": name, "fyi_only": fyi_only,
                }),
                std::time::Duration::from_secs(30),
            )?
        }
        LocalAgentAction::Watch => {
            let state = local_state
                .clone()
                .map(Ok)
                .unwrap_or_else(cadence_agent::client::state_dir)?;
            ensure_report_driver(&state);
            let mut previous: Option<Value> = None;
            loop {
                let value = cadence_agent::client::rpc_timeout(
                    &state,
                    "hosted_local_agent_observe",
                    json!({
                        "organization_id": target.org_id,
                        "after": previous.as_ref().and_then(|value| value["cursor"].as_u64()),
                        "wait": previous.is_some(),
                    }),
                    std::time::Duration::from_secs(35),
                )?;
                if previous.as_ref() != Some(&value) {
                    print_json(&value);
                    previous = Some(value);
                }
            }
        }
        LocalAgentAction::Disconnect => {
            let state = local_state
                .clone()
                .map(Ok)
                .unwrap_or_else(cadence_agent::client::state_dir)?;
            ensure_report_driver(&state);
            let value = cadence_agent::client::rpc(
                &state,
                "hosted_local_agent_disconnect",
                json!({"organization_id": target.org_id}),
            )?;
            coord.disconnect()?;
            value
        }
        LocalAgentAction::Status => {
            let mut value = coord.status()?;
            let state = local_state
                .clone()
                .map(Ok)
                .unwrap_or_else(cadence_agent::client::state_dir)?;
            value["inbox"] =
                cadence_agent::local_assignment::events::observe(&state, &target.org_id, false)?;
            value
        }
        LocalAgentAction::Inbox { clear } => {
            let state = local_state
                .clone()
                .map(Ok)
                .unwrap_or_else(cadence_agent::client::state_dir)?;
            // Observation/clear never authorizes starting actors or pickup.
            // A stopped daemon is reported by the observer RPC unchanged.
            cadence_agent::client::rpc(
                &state,
                "hosted_local_agent_observe",
                json!({"organization_id": target.org_id, "clear": clear}),
            )?
        }
        LocalAgentAction::Report {
            assignment,
            state,
            text,
            head,
        } => {
            let value = coord.report(assignment, state, text.as_deref(), head.as_deref())?;
            if *state == "result" {
                // The assignment ends here; the ticket does not move on its own.
                // Say the next step instead of leaving the session to infer it.
                eprintln!(
                    "cadence: assignment complete — close the ticket with \
                     `cadence issue set <ID> status=done` once the work is accepted"
                );
            }
            value
        }
        LocalAgentAction::Reassign {
            assignment,
            to,
            cancel,
        } => coord.reassign(assignment, to.as_deref(), *cancel)?,
        LocalAgentAction::Tool { lane } => coord.authorize_tool(lane)?,
    };
    finish(&value)
}

pub(super) fn dispatch(target: &RemoteTarget, issue: &str, to: &str) -> Result<i32> {
    finish(&coordinator(target)?.dispatch(issue, Some(to))?)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn start(
    target: &RemoteTarget,
    id: &str,
    repo: Option<&Path>,
    name: Option<&str>,
    profile: Option<&str>,
    base: Option<&str>,
    wake_timeout: u64,
    local_state: Option<PathBuf>,
) -> Result<i32> {
    let Some(repo) = repo else {
        return Err(Error::usage(
            "hosted issue start requires --repo <local git checkout>; nothing was sent",
        ));
    };
    if let Some(profile) = profile {
        // CAD-1964: the caller is the actor, so this claims under the
        // profile's session id and opens the lane without a local alias,
        // a daemon handoff or any other waiting assignment.
        return finish(&coordinator(target)?.start_session(id, repo, name, profile, base)?);
    }
    if base.is_some() {
        return Err(Error::usage(
            "hosted issue start accepts --base only with --profile; the alias path uses the              project's own base. Nothing was sent",
        ));
    }
    let mut deliver =
        |kickoff: &Kickoff| deliver_kickoff(target, local_state.clone(), kickoff, wake_timeout);
    let value = coordinator(target)?.start_here(id, repo, name, &mut deliver)?;
    if let Some(dir) = local_state.or_else(|| cadence_agent::client::state_dir().ok()) {
        ensure_report_driver(&dir);
    }
    finish(&value)
}

fn ensure_report_driver(state: &Path) {
    if cadence_agent::client::rpc_timeout(
        state,
        "health",
        json!({}),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
    {
        return;
    }
    let _ = cadence_agent::client::daemon_start(state);
}

fn finish(value: &Value) -> Result<i32> {
    print_json(value);
    if value
        .get("refused")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty())
    {
        return Ok(5);
    }
    Ok(0)
}

#[cfg(test)]
mod context_snapshots {
    use super::*;

    #[test]
    fn a_new_generation_cannot_overwrite_an_older_actor_context() {
        let dir = tempfile::tempdir().expect("state");
        let mut kickoff = Kickoff {
            alias: "worker",
            issue_id: "CAD1460",
            assignment_id: "hla_00000000000000000000000000000001",
            worktree: "/tmp/cad1460-work",
            message_id: "0123456789abcdef0123456789abcdef",
            claim_id: "hlc_00000000000000000000000000000001",
            generation: 1,
            organization_id: "ws_cad1460",
            audience: "https://board.test",
            subject_id: None,
            issuer: None,
        };
        let first = write_note(dir.path(), &kickoff, &json!({"issue": "original work"}))
            .expect("first context");
        let original = fs::read(&first).expect("original snapshot");
        kickoff.message_id = "abcdef0123456789abcdef0123456789";
        kickoff.claim_id = "hlc_00000000000000000000000000000002";
        kickoff.generation = 2;
        let second = write_note(dir.path(), &kickoff, &json!({"issue": "reassigned work"}))
            .expect("replacement context");
        assert_ne!(first, second);
        assert_eq!(fs::read(first).expect("older actor context"), original);
    }

    #[test]
    fn redaction_preserves_org_selectors_but_hides_complete_credentials() {
        let credential = format!("hct_{}", "a".repeat(43));
        let redacted = redact_secrets(format!(
            "org=hct_company\ncredential={credential}\nwiki=global/hct_notes.md"
        ));
        assert!(redacted.contains("org=hct_company\n"));
        assert!(redacted.contains("wiki=global/hct_notes.md"));
        assert!(!redacted.contains(&credential));
    }
}

#[cfg(test)]
#[path = "local_agent_claimant_acceptance.rs"]
mod claimant_acceptance;
