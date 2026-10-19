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
    #[command(visible_alias = "list")]
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
        /// CAD-225: issue a bounded preauthorization — ONE admission of
        /// ONE exact render of this workflow (`--repo`/`--allow-path`
        /// bound; defaults expire in 3600s). Absent means ordinary
        /// approval and replaces any prior grant.
        #[arg(long, conflicts_with = "renew")]
        unattended: bool,
        /// CAD-225: renew the grant named by this id — same immutable
        /// bounds, fresh expiry. Conflicts with every other field.
        #[arg(long, conflicts_with_all = ["unattended", "repo", "allow_path", "input"])]
        renew: Option<String>,
        /// CAD-225: bound repo `OWNER/REPO`; required with --unattended.
        #[arg(long)]
        repo: Option<String>,
        /// CAD-225: normalized repo-relative allowlist entry; repeatable,
        /// at least one required with --unattended.
        #[arg(long = "allow-path")]
        allow_path: Vec<String>,
        /// CAD-225: bind one exact render — `--input k=v`, repeatable.
        #[arg(long = "input")]
        input: Vec<String>,
        /// CAD-225: grant lifetime in seconds (1-86400); with
        /// --unattended or --renew only.
        #[arg(long)]
        ttl: Option<u64>,
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
        WorkflowAction::Approve {
            name,
            project,
            unattended,
            renew,
            repo,
            allow_path,
            input,
            ttl,
        } => {
            // CAD-225: k=v inputs mirror `plan propose --input` — the
            // grant binds this exact map, so an explicit `k=` is bound
            // as "" even when the template default differs.
            let mut inputs = serde_json::Map::new();
            for kv in input {
                let Some((k, v)) = kv.split_once('=') else {
                    return Err(Error::rejected(format!("--input '{kv}' — use --input k=v")));
                };
                if k.is_empty() {
                    return Err(Error::rejected("--input needs a non-empty key — k=v"));
                }
                if inputs
                    .insert(k.to_string(), Value::String(v.to_string()))
                    .is_some()
                {
                    return Err(Error::rejected(format!("--input '{k}' is given twice")));
                }
            }
            let mut params = json!({"project": project, "name": name});
            if *unattended {
                // `inputs` is a required key of the strict grant schema —
                // an empty map still binds "the default render". Absent
                // values are OMITTED, never sent as null: the daemon
                // treats a present key (even null) as a supplied field.
                params["unattended"] = json!({
                    "paths": allow_path,
                    "inputs": Value::Object(inputs),
                });
                if let Some(repo) = repo {
                    params["unattended"]["repo"] = json!(repo);
                }
                if let Some(secs) = ttl {
                    params["unattended"]["ttl_secs"] = json!(secs);
                }
            } else {
                if let Some(grant_id) = renew {
                    params["renew"] = json!(grant_id);
                }
                if let Some(secs) = ttl {
                    params["ttl_secs"] = json!(secs);
                }
                if repo.is_some() {
                    params["repo"] = json!(repo);
                }
                if !allow_path.is_empty() {
                    params["paths"] = json!(allow_path);
                }
                if !inputs.is_empty() {
                    params["inputs"] = Value::Object(inputs);
                }
            }
            client::rpc(state_dir, "workflow_approve", params)?
        }
    };
    print_json(&result);
    Ok(0)
}

pub(super) fn run(state_dir: PathBuf, action: WorkflowAction) -> Result<i32> {
    run_workflow(&state_dir, action)
}
