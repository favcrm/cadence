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
    /// Read-only (CAD-1106): the ticket scope pre-approvals recorded for
    /// `--issue` (id, body digest, how recorded, revoked or not). Prints one
    /// JSON object; exits 0 when the store answered. Never records.
    Scope {
        /// Tracker issue id the scope approvals name.
        #[arg(long)]
        issue: String,
    },
    /// Read-only (CAD-959): the verdicts of ISSUE that name PR `--pr` and the
    /// full `--head`, plus a `skipped` list with the reason for every note of
    /// the issue that does not bind. The org this invocation selects decides
    /// the source, exactly as `--org`/`CADENCE_ORG` route every other verb: a
    /// local org parses `--notes-dir` with the audit's own note parser, a
    /// hosted org reads its own record and never this machine's notes.
    /// Reviewer identity is the FIRST whitespace token of `From:`,
    /// lower-cased (the rest is prose). One JSON document.
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
        /// Notes directory (local orgs; default /var/www/agent-notes).
        #[arg(long)]
        notes_dir: Option<PathBuf>,
    },
    /// Record one reviewer's verdict for exactly this issue, PR and head
    /// (hosted orgs). The board takes the reviewer from the authenticated
    /// caller, never from an argument; a second verdict of the same kind for
    /// the same head returns the first and rewrites nothing.
    Verdict {
        /// Tracker issue id the verdict names.
        #[arg(long)]
        issue: String,
        /// PR number the verdict pins.
        #[arg(long)]
        pr: u64,
        /// Full 40-hex head SHA the verdict names — a new head voids it.
        #[arg(long)]
        head: String,
        /// `standards` or `spec`.
        #[arg(long)]
        kind: String,
        /// `pass` or `revise`.
        #[arg(long)]
        outcome: String,
        /// The title's outcome; defaults to `--outcome`.
        #[arg(long = "title-outcome")]
        title_outcome: Option<String>,
        /// The findings section outcome; defaults to `--outcome`.
        #[arg(long = "section-outcome")]
        section_outcome: Option<String>,
        /// The risk line the note states (`auto`, or `human (…)`).
        #[arg(long, default_value = "auto")]
        risk: String,
        /// Findings text; `-` reads stdin.
        #[arg(long, value_name = "PATH", conflicts_with = "text")]
        file: Option<PathBuf>,
        /// Findings text inline.
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
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
        // A verdict is a hosted record: the board owns the reviewer identity
        // and the exact-head binding. The native path files note files under
        // the notes dir instead, which this verb never fabricates.
        Some(AuditAction::Verdict { .. }) => Err(Error::rejected(
            "audit verdict records a verdict in the hosted org; select it with `--org <org>`.              Nothing was written",
        )),
        // Dispatched before the state dir resolved: `audit verdicts` routes
        // through the org resolver there (CAD-1981), so a hosted selection
        // never reaches this local-only reader.
        Some(AuditAction::Verdicts { .. }) => {
            unreachable!("audit verdicts is dispatched before state-dir")
        }
        Some(AuditAction::Scope { issue }) => {
            let (v, code) = cadence_agent::audit::scope_check(&state_dir, issue.trim());
            print_json(&v);
            Ok(code)
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

/// `cadence audit verdicts` for a local selection: a pure parse of the
/// notes directory. It needs no daemon, state dir or `HOME`, and the
/// dispatcher reaches it before any of those resolve; a hosted selection
/// never reaches here and answers through the org's own read (CAD-1946,
/// CAD-1981).
pub(super) fn verdicts(
    issue: &str,
    pr: u64,
    head: &str,
    notes_dir: Option<&std::path::Path>,
) -> Result<i32> {
    let head = full_head(head)?;
    print_json(&cadence_agent::audit::verdicts_check(
        notes_dir,
        issue.trim(),
        pr,
        &head,
    )?);
    Ok(0)
}

/// A pinned head is the full 40-hex SHA, lower-cased; anything shorter
/// would let a read bind to another revision.
pub(super) fn full_head(head: &str) -> Result<String> {
    let h = head.trim().to_ascii_lowercase();
    if h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(h)
    } else {
        Err(Error::rejected("--head must be the full 40-hex SHA"))
    }
}
