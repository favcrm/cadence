//! CAD-535: `cadence audit` — moved verbatim from src/main.rs.

use super::*;

/// Operator approval evidence for `cadence audit` (CAD-217). Both
/// verbs go through the daemon, which refuses any agent connection;
/// the records grant nothing — `cadence audit` binds them to merges.
#[derive(Subcommand)]
pub(crate) enum AuditAction {
    /// Record that the operator approved `--action` (default merge) on
    /// the exact full `--head` of PR `--pr`. `--delegated`: a designated
    /// agent records a delegated approval from its own pane (CAD-918).
    /// `--issue X --action scope`: the operator pre-approves a ticket's
    /// scope at ticket time.
    Approve {
        /// PR number the approval covers.
        #[arg(long, required_unless_present = "issue", conflicts_with = "issue")]
        pr: Option<u64>,
        /// Full 40-hex head SHA the approval names — an approval never
        /// carries over to a later head.
        #[arg(long, required_unless_present = "issue")]
        head: Option<String>,
        /// The ticket whose scope `--action scope` pre-approves.
        #[arg(long, requires = "action")]
        issue: Option<String>,
        /// Record a delegated approval: the daemon checks every
        /// safeguard and records it under this pane's own agent.
        #[arg(long, conflicts_with_all = ["issue", "id"])]
        delegated: bool,
        /// The scope pre-approval id a delegated approval relies on.
        #[arg(long, requires = "delegated")]
        scope: Option<String>,
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
    /// Read-only (CAD-959): is a merge approval in force for exactly this
    /// full `--head` of PR `--pr`? Prints one JSON object; exits 0 only
    /// for state `in-force`. Reads the store directly and never records.
    Approval {
        /// PR number.
        #[arg(long)]
        pr: u64,
        /// Full 40-hex head SHA — an approval for another head never counts.
        #[arg(long)]
        head: String,
        /// `owner/name` (default: the cwd checkout's github.com origin).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Read-only (CAD-959): the verdict notes of ISSUE that name PR `--pr`
    /// and the full `--head`, parsed by the audit's own note parser, plus
    /// a `skipped` list with the reason for every note of the issue that
    /// does not bind. Reviewer identity is the FIRST whitespace token of
    /// `From:`, lower-cased (the rest is prose). One JSON document.
    Verdicts {
        /// Tracker issue id the notes belong to (`Issue:` header).
        #[arg(long)]
        issue: String,
        /// PR number the notes must name.
        #[arg(long)]
        pr: u64,
        /// Full 40-hex head SHA the notes must pin.
        #[arg(long)]
        head: String,
        /// Notes directory (default /var/www/agent-notes).
        #[arg(long)]
        notes_dir: Option<PathBuf>,
    },
    /// Designate an agent to record delegated approvals for a project
    /// (operator only; `--revoke` withdraws the designation).
    Designate {
        alias: String,
        #[arg(long)]
        project: String,
        /// Who designated and where.
        #[arg(long)]
        source: String,
        #[arg(long)]
        revoke: bool,
    },
    /// List the designations in force.
    Designations,
    /// Delegated approvals in a window (JSON), each with its revoke and
    /// revert commands. Read-only.
    Digest {
        /// 24h (default), 7d, YYYY-MM-DD or epoch.
        #[arg(long)]
        since: Option<String>,
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
        Some(AuditAction::Verdicts {
            issue,
            pr,
            head,
            notes_dir,
        }) => {
            let head = full_head(&head)?;
            print_json(&cadence_agent::audit::verdicts_check(
                notes_dir.as_deref(),
                issue.trim(),
                pr,
                &head,
            )?);
            Ok(0)
        }
        Some(AuditAction::Approval { pr, head, repo }) => {
            let head = full_head(&head)?;
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
            let (v, code) = cadence_agent::audit::approval_check(&state_dir, &repo, pr, &head);
            print_json(&v);
            Ok(code)
        }
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

/// A pinned head is the full 40-hex SHA, lower-cased; anything shorter
/// would let a read bind to another revision.
fn full_head(head: &str) -> Result<String> {
    let h = head.trim().to_ascii_lowercase();
    if h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(h)
    } else {
        Err(Error::rejected("--head must be the full 40-hex SHA"))
    }
}
