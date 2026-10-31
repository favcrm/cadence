//! CAD-535: `cadence plan` — moved verbatim from src/main.rs.

use super::*;

/// `cadence plan` verbs. Writes go through the daemon, which
/// attributes the proposer from the connection and refuses any agent
/// connection for a decision.
#[derive(Subcommand)]
pub(crate) enum PlanAction {
    /// Create an epic (the plan, `proposed`) and one backlog ticket per
    /// `## <title>` section of `--file`, in one tracker commit. With
    /// `--workflow` the file is the project's stored template, rendered
    /// with `--input k=v` first (CAD-487).
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
    /// may dispatch. Operator only.
    Approve {
        /// The plan's epic id.
        epic: String,
    },
    /// Reject a proposed plan. Operator only.
    Reject {
        /// The plan's epic id.
        epic: String,
        /// Why — recorded on the epic.
        #[arg(long)]
        reason: String,
    },
    /// Plan state, tickets with status, and size-weighted progress
    /// (S=1 M=3 L=8, unsized=M). Read-only.
    Show {
        /// The plan's epic id.
        epic: String,
    },
    /// Every plan — one row per epic carrying one: state, proposer,
    /// decision, ticket ids, size-weighted progress. Value flags
    /// repeat and comma-join and match ANY of their values; different
    /// flags AND.
    #[command(after_long_help = cadence_agent::filter::GRAMMAR)]
    #[command(visible_alias = "list")]
    Ls {
        /// Plan state (proposed approved rejected); repeatable.
        #[arg(long, value_delimiter = ',')]
        state: Vec<String>,
        /// Project key; repeatable.
        #[arg(long, value_delimiter = ',')]
        project: Vec<String>,
        /// Sort by id project title status state proposed_by
        /// proposed_at progress; `-KEY` descending.
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
}

pub(super) fn run_plan(state_dir: &Path, action: PlanAction) -> Result<i32> {
    let result = match action {
        PlanAction::Propose {
            project,
            file,
            workflow,
            inputs,
        } => {
            return cadence_agent::issue::cli::propose_plan(
                state_dir,
                &project,
                file.as_deref(),
                workflow.as_deref(),
                &inputs,
                "plan",
            );
        }
        PlanAction::Approve { epic } => {
            return cadence_agent::issue::cli::decide_plan(state_dir, &epic, None);
        }
        PlanAction::Reject { epic, reason } => {
            return cadence_agent::issue::cli::decide_plan(state_dir, &epic, Some(&reason));
        }
        PlanAction::Show { epic } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            cadence_agent::issue::plan::show(&pm, &epic)?
        }
        PlanAction::Ls {
            state,
            project,
            sort,
            limit,
            fields,
            json: _,
        } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            cadence_agent::issue::plan::ls(&pm, &state, &project, sort.as_deref(), limit, &fields)?
        }
    };
    print_json(&result);
    Ok(0)
}

pub(super) fn run(state_dir: PathBuf, action: PlanAction) -> Result<i32> {
    run_plan(&state_dir, action)
}
