//! CAD-535: `cadence workflow` — moved verbatim from src/main.rs.

use super::*;

/// `cadence workflow` verbs (CAD-487). `add`/`edit` write the file in
/// one tracker commit with the actor recorded — the only writer;
/// `check`/`ls`/`show` read; `approve` is the operator's gate.
#[derive(Subcommand)]
pub(crate) enum WorkflowAction {
    /// Store `<file>` as the project's `<name>` workflow — refuses if
    /// it exists (use `edit`) or fails `workflow check`.
    Add {
        /// Workflow name — 1-32 lowercase letters, digits or hyphens;
        /// the file lands at `<pm>/<project>/workflows/<name>.md`.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
        /// The workflow file; `-` reads stdin.
        #[arg(long)]
        file: PathBuf,
    },
    /// Replace a stored workflow — the file must exist and pass
    /// `workflow check`. An approval-affecting change (agent,
    /// depends_on, size, reviewer, tries, uses, a ticket added or
    /// dropped) unapproves it until `workflow approve` runs again;
    /// wording-only edits keep approval.
    Edit {
        /// Workflow name.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
        /// The new content; `-` reads stdin.
        #[arg(long)]
        file: PathBuf,
    },
    /// Validate a workflow: parses, every `agent` exists, `depends_on`
    /// has no cycle, every ticket has acceptance, every `{{name}}` is
    /// a declared input, no reviewer is the ticket's own agent.
    /// Non-zero exit on any refusal. `<target>` is a file path or a
    /// name in `--project`'s workflows/.
    Check {
        /// A file path (…/x.md) or a stored workflow name.
        target: String,
        /// Project key — the name form needs it; the file form uses it
        /// (or the cwd's project) to resolve `agent:` names.
        #[arg(long)]
        project: Option<String>,
    },
    /// Every stored workflow: project, name, inputs, approval state.
    Ls {
        /// Project key; all projects when absent.
        #[arg(long)]
        project: Option<String>,
    },
    /// A stored workflow's summary: title, inputs, tickets, gate
    /// digest and approval state.
    Show {
        /// Workflow name.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Approve the workflow's current gate keys — `plan propose
    /// --workflow` refuses it until this matches the file on disk.
    /// Operator only, through the daemon.
    Approve {
        /// Workflow name.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
}

/// `cadence workflow …` (CAD-487). `add`/`edit` write the tracker
/// directly — one commit each, `Actor:` recorded — and `check`/`ls`/
/// `show` read it; `approve` is the operator's daemon call, like
/// `issue project approve-work`. `check` exits non-zero on any
/// refusal, like `issue doctor`.
pub(super) fn run_workflow(state_dir: &Path, action: WorkflowAction) -> Result<i32> {
    use cadence_agent::issue::workflow;
    let result = match &action {
        WorkflowAction::Add {
            name,
            project,
            file,
        }
        | WorkflowAction::Edit {
            name,
            project,
            file,
        } => {
            let cap = cadence_agent::issue::plan::MAX_PLAN_BYTES as u64;
            let text = read_body_capped(None, Some(file.clone()), cap).map_err(|e| {
                Error::rejected(format!("Cannot read workflow {}: {e}", file.display()))
            })?;
            let pm = cadence_agent::issue::Pm::open_default()?;
            workflow::write_file(
                &pm,
                project,
                name,
                &text,
                matches!(action, WorkflowAction::Add { .. }),
                state_dir,
                "",
            )?
        }
        WorkflowAction::Check { target, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            let report = workflow::check(&pm.dir, target, project.as_deref(), state_dir)?;
            if report["ok"].as_bool() == Some(true) {
                print_json(&report);
                return Ok(0);
            }
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
            return Ok(1);
        }
        WorkflowAction::Ls { project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            workflow::ls(&pm, project.as_deref(), state_dir)?
        }
        WorkflowAction::Show { name, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            workflow::show(&pm, project, name, state_dir)?
        }
        WorkflowAction::Approve { name, project } => client::rpc(
            state_dir,
            "workflow_approve",
            json!({"project": project, "name": name}),
        )?,
    };
    print_json(&result);
    Ok(0)
}

pub(super) fn run(state_dir: PathBuf, action: WorkflowAction) -> Result<i32> {
    run_workflow(&state_dir, action)
}
