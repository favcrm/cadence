//! CAD-2317: `cadence routine` — recurring schedules and verified app event
//! triggers targeting versioned Context Markdown (`wiki:` / `workflows/`) or
//! `sha256`-pinned scripts, plus staged assistant proposals.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

mod args;

use std::path::{Path, PathBuf};

use cadence_agent::client;
use cadence_agent::error::{Error, Result};
use serde_json::{json, Value};

use super::print_json;
pub(crate) use args::RoutineAction;

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
