//! CAD-2317: `cadence routine` — recurring schedules and verified app event
//! triggers targeting versioned Context Markdown (`wiki:` / `workflows/`) or
//! `sha256`-pinned scripts, plus staged assistant proposals.

use super::*;

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
        /// Explicit IANA timezone (e.g. `Asia/Hong_Kong`, `UTC`).
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

fn resolve_target_args(
    markdown: &Option<String>,
    script: &Option<String>,
    then_markdown: &Option<String>,
) -> Result<(&'static str, Option<String>, Option<String>)> {
    match (script, then_markdown, markdown) {
        (_, Some(_), Some(_)) => Err(Error::usage(
            "pass either `--markdown` (standalone playbook) or `--script ... --then-markdown ...`, not both",
        )),
        (Some(sp), Some(md), None) => {
            Ok(("script_then_markdown", Some(md.clone()), Some(sp.clone())))
        }
        (Some(sp), None, None) => Ok(("script", None, Some(sp.clone()))),
        (None, None, Some(md)) => Ok(("markdown", Some(md.clone()), None)),
        (Some(_), None, Some(_)) => Err(Error::usage(
            "use `--then-markdown <path>` with `--script <path>` to wake the agent after a script pre-check",
        )),
        (None, Some(_), None) => Err(Error::usage(
            "`--then-markdown` requires `--script <path>`",
        )),
        (None, None, None) => Err(Error::usage(
            "routine requires `--markdown <wiki-or-workflow-path>` or `--script <path>`",
        )),
    }
}

pub(crate) fn run(state_dir: PathBuf, action: RoutineAction) -> Result<i32> {
    run_routine(&state_dir, &action)
}

fn run_routine(state_dir: &Path, action: &RoutineAction) -> Result<i32> {
    let rpc = |method: &str, params: Value| client::rpc(state_dir, method, params);
    match action {
        RoutineAction::Create {
            name,
            agent,
            cron,
            interval_mins,
            tz,
            on_event,
            markdown,
            script,
            then_markdown,
            thread,
            connection,
        } => {
            let (target_kind, md_path, sp_path) =
                resolve_target_args(markdown, script, then_markdown)?;
            let trigger_kind = if on_event.is_some() {
                "event"
            } else {
                "schedule"
            };
            print_json(&rpc(
                "routine_create",
                json!({
                    "name": name,
                    "agent_alias": agent,
                    "trigger_kind": trigger_kind,
                    "cron": cron,
                    "interval_mins": interval_mins,
                    "timezone": tz,
                    "event_source": on_event,
                    "target_kind": target_kind,
                    "markdown_path": md_path,
                    "script_path": sp_path,
                    "destination_thread": thread,
                    "connection_account": connection,
                }),
            )?);
        }
        RoutineAction::List { agent } => {
            let mut params = json!({});
            if let Some(a) = agent {
                params["agent_alias"] = json!(a);
            }
            print_json(&rpc("routine_list", params)?);
        }
        RoutineAction::Show { id } => {
            print_json(&rpc("routine_show", json!({ "id": id }))?);
        }
        RoutineAction::Edit {
            id,
            name,
            cron,
            interval_mins,
            tz,
            on_event,
            markdown,
            script,
            then_markdown,
            thread,
            connection,
        } => {
            if on_event.is_some() && (cron.is_some() || interval_mins.is_some()) {
                return Err(Error::usage(
                    "pass either `--on <event>` or `--cron` / `--interval-mins`, not both",
                ));
            }
            let mut params = json!({ "id": id });
            if let Some(v) = name {
                params["name"] = json!(v);
            }
            if let Some(v) = cron {
                params["trigger_kind"] = json!("schedule");
                params["cron"] = json!(v);
                params["interval_mins"] = Value::Null;
                params["event_source"] = Value::Null;
            }
            if let Some(v) = interval_mins {
                params["trigger_kind"] = json!("schedule");
                params["interval_mins"] = json!(v);
                params["cron"] = Value::Null;
                params["event_source"] = Value::Null;
            }
            if let Some(v) = tz {
                params["timezone"] = json!(v);
            }
            if let Some(v) = on_event {
                params["trigger_kind"] = json!("event");
                params["event_source"] = json!(v);
                params["cron"] = Value::Null;
                params["interval_mins"] = Value::Null;
            }
            if markdown.is_some() || script.is_some() || then_markdown.is_some() {
                let (tk, md, sp) = resolve_target_args(markdown, script, then_markdown)?;
                params["target_kind"] = json!(tk);
                params["markdown_path"] = json!(md);
                params["script_path"] = json!(sp);
            }
            if let Some(v) = thread {
                params["destination_thread"] = if v.is_empty() { Value::Null } else { json!(v) };
            }
            if let Some(v) = connection {
                params["connection_account"] = if v.is_empty() { Value::Null } else { json!(v) };
            }
            print_json(&rpc("routine_update", params)?);
        }
        RoutineAction::Approve {
            id,
            expect_rev,
            digest,
        } => {
            let mut params = json!({ "id": id });
            if let Some(r) = expect_rev {
                params["expect_rev"] = json!(r);
            }
            if let Some(d) = digest {
                params["digest"] = json!(d);
            }
            print_json(&rpc("routine_approve", params)?);
        }
        RoutineAction::Run {
            id,
            due_instant,
            trigger_source,
        } => {
            let mut params = json!({
                "id": id,
                "trigger_source": trigger_source,
            });
            if let Some(due) = due_instant {
                params["due_instant"] = json!(due);
            }
            print_json(&rpc("routine_run", params)?);
        }
        RoutineAction::Pause { id, reason } => {
            let mut params = json!({ "id": id });
            if let Some(r) = reason {
                params["reason"] = json!(r);
            }
            print_json(&rpc("routine_pause", params)?);
        }
        RoutineAction::Resume { id } => {
            print_json(&rpc("routine_resume", json!({ "id": id }))?);
        }
        RoutineAction::Remove { id } => {
            print_json(&rpc("routine_remove", json!({ "id": id }))?);
        }
        RoutineAction::Propose {
            name,
            agent,
            cron,
            interval_mins,
            tz,
            on_event,
            markdown,
            script,
            then_markdown,
        } => {
            let (target_kind, md_path, sp_path) =
                resolve_target_args(markdown, script, then_markdown)?;
            let trigger_kind = if on_event.is_some() {
                "event"
            } else {
                "schedule"
            };
            print_json(&rpc(
                "routine_propose",
                json!({
                    "name": name,
                    "agent_alias": agent,
                    "trigger_kind": trigger_kind,
                    "cron": cron,
                    "interval_mins": interval_mins,
                    "timezone": tz,
                    "event_source": on_event,
                    "target_kind": target_kind,
                    "markdown_path": md_path,
                    "script_path": sp_path,
                }),
            )?);
        }
        RoutineAction::RunDecide {
            run_id,
            approve,
            dismiss,
        } => {
            let decision = match (approve, dismiss) {
                (true, false) => "approve",
                (false, true) => "dismiss",
                _ => {
                    return Err(Error::usage(
                        "pass exactly one of `--approve` or `--dismiss`",
                    ))
                }
            };
            print_json(&rpc(
                "routine_run_decide",
                json!({
                    "run_id": run_id,
                    "decision": decision,
                }),
            )?);
        }
        RoutineAction::ProposalDecide {
            proposal_id,
            approve,
            dismiss,
        } => {
            let decision = match (approve, dismiss) {
                (true, false) => "approve",
                (false, true) => "dismiss",
                _ => {
                    return Err(Error::usage(
                        "pass exactly one of `--approve` or `--dismiss`",
                    ))
                }
            };
            print_json(&rpc(
                "routine_proposal_decide",
                json!({
                    "proposal_id": proposal_id,
                    "decision": decision,
                }),
            )?);
        }
    }
    Ok(0)
}
