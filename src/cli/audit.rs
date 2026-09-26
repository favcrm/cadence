//! CAD-535: `cadence audit` — moved verbatim from src/main.rs.

use super::*;

/// Operator approval evidence for `cadence audit` (CAD-217). Both
/// verbs go through the daemon, which refuses any agent connection;
/// the records grant nothing — `cadence audit` binds them to merges.
#[derive(Subcommand)]
pub(crate) enum AuditAction {
    /// Record that the operator approved `--action` (default merge) on
    /// the exact full `--head` of PR `--pr`.
    Approve {
        /// PR number the approval covers.
        #[arg(long)]
        pr: u64,
        /// Full 40-hex head SHA the approval names — an approval never
        /// carries over to a later head.
        #[arg(long)]
        head: String,
        /// Who approved and where (e.g. "chris in chat 22:33Z"). A claim
        /// the record carries; never `user` or `daemon`.
        #[arg(long)]
        source: String,
        /// `owner/name` (default: the cwd checkout's github.com origin).
        #[arg(long)]
        repo: Option<String>,
        /// The approved action.
        #[arg(long, default_value = "merge")]
        action: String,
        /// Stable id for retries and a later revoke (default
        /// `<action>-pr<N>-<head[..12]>`, then `-2`, `-3`, … once an
        /// earlier default was revoked). A revoked id is never reused.
        #[arg(long)]
        id: Option<String>,
    },
    /// Withdraw an approval id. Cancelling a message never does this.
    Revoke {
        /// The approval id `audit approve` recorded.
        id: String,
        /// Who revoked and where.
        #[arg(long)]
        source: String,
        /// Why the approval no longer holds.
        #[arg(long)]
        reason: String,
    },
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    action: Option<AuditAction>,
    since: Option<String>,
    class: Option<String>,
    project: Option<String>,
    json: bool,
    limit: Option<u64>,
    repo: Option<PathBuf>,
    notes_dir: Option<PathBuf>,
    merge_report: Option<PathBuf>,
) -> Result<i32> {
    match action {
        Some(action) => run_audit_evidence(&state_dir, action),
        None => cadence_agent::audit::run(&cadence_agent::audit::AuditOptions {
            since,
            class,
            project,
            json,
            limit,
            repo,
            notes_dir,
            merge_report,
            cwd: std::env::current_dir()?,
            state_dir,
        }),
    }
}
