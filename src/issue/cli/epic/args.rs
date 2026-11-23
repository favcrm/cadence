//! CLI subcommand definitions for `cadence issue epic`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum EpicAction {
    /// List epics with `total`, per-status counts, `done_ratio`,
    /// `blocked`, the distinct owners of their children and a `work`
    /// block: stage, size-weighted progress (S=1 M=3 L=8, unsized=M,
    /// dropped excluded) and health (on_track | at_risk | stalled).
    /// Value flags repeat and comma-join and match ANY of their values;
    /// different flags AND.
    #[command(after_long_help = crate::filter::GRAMMAR)]
    #[command(visible_alias = "list")]
    Ls {
        /// Project key; repeatable — epics in any of them.
        #[arg(long, value_delimiter = ',')]
        project: Vec<String>,
        /// Stage id (the project's `stages:` list, or shape build
        /// verify release done, plus rejected); repeatable.
        #[arg(long, value_delimiter = ',')]
        stage: Vec<String>,
        /// Health (on_track at_risk stalled); repeatable.
        #[arg(long, value_delimiter = ',')]
        health: Vec<String>,
        /// Milestone id; repeatable.
        #[arg(long, value_delimiter = ',')]
        milestone: Vec<String>,
        /// Sort by id project title status priority owner total
        /// done_ratio blocked stage health progress; `-KEY` descending.
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Keep only the first N rows.
        #[arg(long)]
        limit: Option<usize>,
        /// Keep only these keys in each JSON row (comma-joined).
        /// Refused when stdout is a table; a pipe or `--json` is enough.
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// One epic and its children: status, owner, priority, tags.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Move an epic to another stage — a gate decision and one tracker
    /// commit, through the daemon. One stage forward at a time, any
    /// stage back. A forward move into an operator stage (default
    /// `build` and `release`; PROJECT.md `operator_stages`) needs the
    /// operator's own connection; other moves take the caller's lane.
    Stage {
        /// The epic id.
        id: String,
        /// Target stage (default list: shape build verify release done).
        stage: String,
        /// Why — one line, recorded in the commit subject.
        #[arg(long)]
        note: Option<String>,
    },
    /// Create an epic (the plan, `proposed`) and one backlog ticket per
    /// `## <title>` section of `--file`, in one tracker commit. With
    /// `--workflow` the file is the project's stored template, rendered
    /// with `--input k=v` first. Same write as `plan propose`.
    Propose {
        /// Project key the plan files into.
        #[arg(long)]
        project: String,
        /// Plan Markdown: frontmatter `title`, `goal`, `non_goals`; one
        /// `## <ticket>` section each with optional `size: S|M|L`,
        /// `agent: <alias>`, `depends_on: 2, CAD-9` lines and a
        /// `### Acceptance` checklist. `-` reads stdin.
        #[arg(long, conflicts_with = "workflow")]
        file: Option<PathBuf>,
        /// Propose the stored workflow `<pm>/<project>/workflows/<name>.md`:
        /// `{{input}}` placeholders take the `--input` values and the
        /// rendered plan lands on the same approve → gate path. The
        /// workflow's gate keys must be operator-approved
        /// (`cadence workflow approve`).
        #[arg(long)]
        workflow: Option<String>,
        /// `k=v` for the workflow's `inputs:` — repeatable; a missing
        /// required input or an unknown name refuses.
        #[arg(long = "input", requires = "workflow")]
        inputs: Vec<String>,
    },
    /// Approve a proposed plan: its backlog tickets move to ready and
    /// may dispatch. Operator only. Same write as `plan approve`.
    Approve {
        /// The plan's epic id.
        epic: String,
    },
    /// Reject a proposed plan. Operator only. Same write as `plan reject`.
    Reject {
        /// The plan's epic id.
        epic: String,
        /// Why — recorded on the epic.
        #[arg(long)]
        reason: String,
    },
}
