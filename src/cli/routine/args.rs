//! Clap CLI argument and subcommand definitions for `cadence routine`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use clap::Subcommand;

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum RoutineAction {
    /// Create a routine in `waiting_approval` until owner approves rev/digest.
    Create {
        /// Human-readable routine name.
        #[arg(long)]
        name: String,
        /// Target agent alias.
        #[arg(long)]
        agent: String,
        /// 5-field cron schedule (>= 15m minimum gap).
        #[arg(long)]
        cron: Option<String>,
        /// Fixed interval in minutes (>= 15).
        #[arg(long)]
        interval_mins: Option<i64>,
        /// IANA timezone (e.g. `Asia/Hong_Kong`, `UTC`). Defaults to the company time zone.
        #[arg(long = "tz", visible_alias = "timezone")]
        tz: Option<String>,
        /// Verified app event source (e.g. `gmail:inbox`), instead of a schedule.
        #[arg(long = "on")]
        on_event: Option<String>,
        /// Versioned Context Markdown path (`wiki:<path>.md` or `[<project>/]workflows/<name>.md`).
        #[arg(long)]
        markdown: Option<String>,
        /// Workspace script path pinned by SHA-256 on approve.
        #[arg(long)]
        script: Option<String>,
        /// Wake agent with this Context Markdown playbook only when `--script` finds items.
        #[arg(long)]
        then_markdown: Option<String>,
        /// Optional destination thread ID.
        #[arg(long)]
        thread: Option<String>,
        /// Optional connection account ID.
        #[arg(long)]
        connection: Option<String>,
    },
    /// List routines (and pending staged proposals), optionally filtered by agent.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show one routine and its recent runs.
    Show { id: String },
    /// Update a routine's schedule, target, or destination thread.
    Edit {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        cron: Option<String>,
        #[arg(long)]
        interval_mins: Option<i64>,
        #[arg(long = "tz", visible_alias = "timezone")]
        tz: Option<String>,
        #[arg(long = "on")]
        on_event: Option<String>,
        #[arg(long)]
        markdown: Option<String>,
        #[arg(long)]
        script: Option<String>,
        #[arg(long)]
        then_markdown: Option<String>,
        #[arg(long)]
        thread: Option<String>,
        #[arg(long)]
        connection: Option<String>,
    },
    /// Owner-approve the routine's live Markdown revision (`--expect-rev`) and/or script `--digest`.
    Approve {
        id: String,
        #[arg(long)]
        expect_rev: Option<String>,
        #[arg(long)]
        digest: Option<String>,
    },
    /// Run an approved routine immediately (enforces live rev & sha256 gates).
    Run {
        id: String,
        #[arg(long)]
        due_instant: Option<i64>,
        #[arg(long, default_value = "manual")]
        trigger_source: String,
    },
    /// Pause a routine.
    Pause {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Resume a paused routine.
    Resume { id: String },
    /// Delete a routine and its run history.
    #[command(visible_alias = "rm")]
    Remove { id: String },
    /// Stage a routine proposal for owner review in `Home` (`To do`).
    Propose {
        #[arg(long)]
        name: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        cron: Option<String>,
        #[arg(long)]
        interval_mins: Option<i64>,
        #[arg(long = "tz", visible_alias = "timezone")]
        tz: Option<String>,
        #[arg(long = "on")]
        on_event: Option<String>,
        #[arg(long)]
        markdown: Option<String>,
        #[arg(long)]
        script: Option<String>,
        #[arg(long)]
        then_markdown: Option<String>,
    },
    /// Approve or dismiss a routine run step waiting for external write/send approval.
    RunDecide {
        run_id: String,
        #[arg(long, conflicts_with = "dismiss")]
        approve: bool,
        #[arg(long, conflicts_with = "approve")]
        dismiss: bool,
    },
    /// Approve or dismiss a staged agent routine or playbook proposal.
    ProposalDecide {
        proposal_id: String,
        #[arg(long, conflicts_with = "dismiss")]
        approve: bool,
        #[arg(long, conflicts_with = "approve")]
        dismiss: bool,
    },
}

impl RoutineAction {
    pub(crate) fn uses_local_script(&self) -> bool {
        match self {
            Self::Create { script, .. }
            | Self::Edit { script, .. }
            | Self::Propose { script, .. } => script.is_some(),
            _ => false,
        }
    }
}
