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
            let mut params = json!({"project": project});
            match (file, workflow) {
                (Some(file), None) => {
                    let cap = cadence_agent::issue::plan::MAX_PLAN_BYTES as u64;
                    let text = match read_master_command_file(state_dir, Some(&file), cap) {
                        Err(e) if e.to_string().contains(cadence_agent::master::NO_STDIN) => {
                            return Err(e);
                        }
                        res => res.map_err(|e| {
                            Error::rejected(format!("Cannot read plan {}: {e}", file.display()))
                        })?,
                    };
                    params["text"] = json!(text);
                }
                (None, Some(name)) => {
                    params["workflow"] = json!(name);
                    if !inputs.is_empty() {
                        let mut map = serde_json::Map::new();
                        for pair in &inputs {
                            let Some((k, v)) = pair.split_once('=') else {
                                return Err(Error::rejected(format!(
                                    "--input '{pair}' — expected k=v"
                                )));
                            };
                            if k.is_empty() {
                                return Err(Error::rejected("--input needs a name: k=v"));
                            }
                            map.insert(k.to_string(), json!(v));
                        }
                        params["inputs"] = Value::Object(map);
                    }
                }
                _ => {
                    if cadence_agent::master::caller_is_master() {
                        return Err(Error::rejected(cadence_agent::master::NO_STDIN));
                    }
                    return Err(Error::rejected(
                        "plan propose needs --file <plan.md> or --workflow <name>",
                    ));
                }
            }
            client::rpc(state_dir, "plan_propose", params)?
        }
        PlanAction::Approve { epic } => {
            client::rpc(state_dir, "plan_approve", json!({"epic": epic}))?
        }
        PlanAction::Reject { epic, reason } => client::rpc(
            state_dir,
            "plan_reject",
            json!({"epic": epic, "reason": reason}),
        )?,
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
