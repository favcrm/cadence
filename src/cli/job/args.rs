//! Clap CLI argument and subcommand definitions for `cadence job`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::PathBuf;

use clap::Subcommand;

use super::super::TaskAction;

#[derive(Subcommand)]
pub(crate) enum JobAction {
    /// Create a job — bookkeeping, not spawning. Requires a registered
    /// PM (any endpoint kind; an inbox alias collects notifications for
    /// `cadence inbox`) and a readable spec file. Writes the job plus
    /// one default task `<job>-t1` covering the spec.
    New {
        /// Owning PM agent — the job's group root.
        #[arg(long)]
        pm: String,
        /// Spec/brief file — hashed at creation for drift detection.
        #[arg(long)]
        spec: PathBuf,
        /// Client idempotency key; same id + same spec hash dedupes.
        #[arg(long)]
        job: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Board issue this job tracks (`<PREFIX>-<n>`, grammar only —
        /// the daemon never reads the board filesystem).
        #[arg(long)]
        issue: Option<String>,
        /// Repo root the job's worktrees live under.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Base branch/SHA QA is relative to.
        #[arg(long)]
        base_ref: Option<String>,
        /// Automatic revision cycles before a revise verdict escalates
        /// the task to blocked.
        #[arg(long, default_value_t = 2)]
        max_revisions: i64,
        /// Silence budget for turns this job's kickoffs start — over it
        /// the PM gets a `turn_stalled` notice (0 disables).
        #[arg(long)]
        stall_secs: Option<u64>,
        /// Title for the default `<job>-t1` task.
        #[arg(long)]
        task_title: Option<String>,
        /// Worktree name (`.cadence/wt/<name>`) scoped onto `<job>-t1`.
        #[arg(long)]
        task_worktree: Option<String>,
        /// Branch scoped onto `<job>-t1`.
        #[arg(long)]
        task_branch: Option<String>,
        /// Base revision scoped onto `<job>-t1`.
        #[arg(long)]
        task_base_sha: Option<String>,
        /// Assignee scoped onto `<job>-t1` — the PM or a group member.
        #[arg(long)]
        task_assignee: Option<String>,
    },
    /// List jobs — non-terminal by default; `--all` widens and a
    /// `--state` selection does too (it names states the default
    /// hides). Repeatable — any of the values.
    #[command(after_long_help = cadence_agent::filter::GRAMMAR)]
    #[command(visible_alias = "ls")]
    List {
        /// draft open done failed cancelled; repeatable — any of them.
        #[arg(long, value_delimiter = ',')]
        state: Vec<String>,
        #[arg(long)]
        all: bool,
        /// Sort by id state title pm issue created updated; `-KEY`
        /// descending.
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Keep only the first N rows.
        #[arg(long)]
        limit: Option<usize>,
        /// Keep only these keys in each row (comma-joined).
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        /// Output is JSON already — accepted for grammar parity.
        #[arg(long)]
        json: bool,
    },
    /// Show a job: tasks with live kickoff state, drift flags, latest
    /// verdicts.
    Show { job: String },
    /// The job-scoped event view — every event any alias row recorded
    /// for this job. Default page is the newest 50; `--after` pages
    /// forward like `cadence events`. A proven operator keeps numeric
    /// sequence cursors; every other reader follows opaque tokens.
    Events {
        job: String,
        /// An opaque cursor from a previous page, or a numeric sequence
        /// on a proven operator page; omit for the newest page.
        #[arg(long)]
        after: Option<String>,
        /// Seconds to wait for new events per request (0-30).
        #[arg(long, default_value_t = 0)]
        wait: u64,
        #[arg(long)]
        follow: bool,
    },
    /// Dispatch a task: enqueue its kickoff to the assignee at a new
    /// revision. Legal from draft/revising and — once the live kickoff
    /// ended without completing — dispatched/running. A live kickoff
    /// makes this an idempotent retry of the same revision.
    Dispatch {
        task: String,
        /// Reassign to another group member (bumps the revision).
        #[arg(long)]
        to: Option<String>,
        /// Claim `agent ready` for the worker first — same operator
        /// claim as `send --ready`; no-op on non-pty endpoints.
        #[arg(long)]
        ready: bool,
        /// Force the ready claim past a busy probe verdict.
        #[arg(long, requires = "ready")]
        force: bool,
        /// Explicit kickoff message id (default: deterministic
        /// cadence-dispatch:<task>:r<n>).
        #[arg(long)]
        message: Option<String>,
        /// Per-dispatch no-provider-event budget in seconds, recorded
        /// on this kickoff message (0 disables). Falls back to
        /// `job new --stall-secs`; a duplicate records nothing.
        #[arg(long)]
        stall_secs: Option<u64>,
    },
    /// Record a QA verdict bound to the task's reported commit.
    /// The reviewer is the verified caller (CAD-372): the agent whose
    /// pane or managed endpoint runs this command, or `operator` from
    /// an operator shell outside every pane. It is never a flag or an
    /// env value. The reviewer can never be the assignee.
    #[command(group = clap::ArgGroup::new("verdict").required(true).args(["pass", "revise", "blocked"]))]
    Verdict {
        task: String,
        /// The commit this verdict judges — must equal the task's
        /// reported head_sha.
        #[arg(long)]
        sha: String,
        /// Approve the revision — task moves to verified.
        #[arg(long)]
        pass: bool,
        /// Request changes — task re-dispatches until max_revisions,
        /// then escalates to blocked.
        #[arg(long)]
        revise: bool,
        /// Stop the task — blocked until an operator reopens it.
        #[arg(long)]
        blocked: bool,
        /// Optional check of who this caller is (`operator` outside a
        /// pane, else the pane's alias). It is never sent: the daemon
        /// derives the reviewer from the connection (CAD-372), and a
        /// mismatch is refused here before anything is written.
        #[arg(long)]
        reviewer: Option<String>,
        /// Evidence file — commands run, outputs, artifact paths.
        #[arg(long)]
        evidence: Option<PathBuf>,
        /// Message id that carried the QA report, if any.
        #[arg(long)]
        message: Option<String>,
        /// Pin the revision this verdict names — a stale value rejects.
        #[arg(long)]
        revision: Option<i64>,
        /// Skip the worktree verification — the opt-out is recorded on
        /// the verdict.
        #[arg(long)]
        no_verify_worktree: bool,
        /// Skip posting the `qa-verdict` commit status to the PR head.
        #[arg(long)]
        no_status: bool,
        /// Post the status to this PR number instead of discovering the
        /// open PR on the task's branch.
        #[arg(long)]
        pr: Option<u64>,
    },
    /// Accept a verified task — records the merge claim. Cadence never
    /// runs git merges itself.
    Accept {
        task: String,
        /// The merge commit once it lands — recorded as evidence.
        #[arg(long)]
        merged_sha: Option<String>,
    },
    /// Cancel every non-terminal task and the job. Queued kickoffs are
    /// cancelled in the same transaction; running ones finish alone.
    /// Agents are never stopped by a job.
    Cancel { job: String },
    /// Close a job — legal only when every task is done.
    Close { job: String },
    /// Manage a job's tasks.
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
}
