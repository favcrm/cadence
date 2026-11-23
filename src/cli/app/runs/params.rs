//! App run parameter builders and ticket execution for `cadence app`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use cadence_agent::client;
use cadence_agent::error::{Error, Result};
use serde_json::{json, Value};

use super::super::RunAction;
use super::read_run_inputs;

pub(in super::super) fn run_params(
    action: &RunAction,
) -> Result<(&'static str, serde_json::Value)> {
    Ok(match action {
        RunAction::Create {
            install_id,
            workflow,
            inputs,
            request_id,
            owner_pm,
            project_link,
            context_id,
            source_receipt_id,
            selected_post_id,
        } => {
            let mut params = json!({
                "install_id": install_id,
                "workflow": workflow,
                "inputs": read_run_inputs(inputs)?,
                "request_id": request_id,
            });
            if let Some(owner) = owner_pm {
                params["owner_pm"] = json!(owner);
            }
            if let Some(link) = project_link {
                params["project_link"] = json!(link);
            }
            if let Some(context) = context_id {
                params["context_id"] = json!(context);
            }
            if let (Some(receipt), Some(post)) = (source_receipt_id, selected_post_id) {
                params["source_receipt_id"] = json!(receipt);
                params["selected_post_id"] = json!(post);
            }
            ("app_run_create", params)
        }
        RunAction::Approve { run_id, digest } => {
            ("app_run_approve", json!({"run_id":run_id,"digest":digest}))
        }
        RunAction::Cancel { run_id } => ("app_run_cancel", json!({"run_id":run_id})),
        RunAction::RetireLegacy { run_id } => ("app_run_retire_legacy", json!({"run_id":run_id})),
        RunAction::Dispatch { run_id } => ("app_run_dispatch", json!({"run_id":run_id})),
        RunAction::Show { run_id } => ("app_run_show", json!({"run_id":run_id})),
        RunAction::Ls {
            install_id,
            context_id,
        } => {
            let mut params = match install_id {
                Some(id) => json!({"install_id":id}),
                None => json!({}),
            };
            if let Some(context) = context_id {
                params["context_id"] = json!(context);
            }
            ("app_run_list", params)
        }
        RunAction::Artifact {
            artifact_id,
            message,
            token,
        } => {
            let params = match (message, token) {
                (None, None) => json!({"artifact_id":artifact_id}),
                (Some(message), Some(token)) => {
                    json!({"artifact_id":artifact_id,"message":message,"token":token})
                }
                _ => {
                    return Err(Error::invalid(
                        "app_run_artifact",
                        "message and token must be supplied together",
                    ));
                }
            };
            ("app_run_artifact", params)
        }
        RunAction::CapabilityCall {
            message,
            token,
            slot,
            request_id,
            input_json,
        } => {
            let input: Value = serde_json::from_str(input_json).map_err(|_| {
                Error::invalid(
                    "app_run_capability_call",
                    "input-json must be a JSON object",
                )
            })?;
            if !input.is_object() || serde_json::to_vec(&input)?.len() > 64 * 1024 {
                return Err(Error::invalid(
                    "app_run_capability_call",
                    "input-json must be an object within 64 KiB",
                ));
            }
            (
                "app_run_capability_call",
                json!({"message":message,"token":token,"slot":slot,
                "request_id":request_id,"input":input}),
            )
        }
        RunAction::CapabilityResults { run_id } => {
            ("app_run_capability_results", json!({"run_id":run_id}))
        }
        RunAction::CapabilityResult {
            receipt_id,
            message,
            token,
        }
        | RunAction::CapabilityAsset {
            receipt_id,
            message,
            token,
        } => {
            let mut params = json!({"receipt_id":receipt_id});
            if let (Some(message), Some(token)) = (message, token) {
                params["message"] = json!(message);
                params["token"] = json!(token);
            }
            (
                if matches!(action, RunAction::CapabilityResult { .. }) {
                    "app_run_capability_result"
                } else {
                    "app_run_capability_asset"
                },
                params,
            )
        }
        RunAction::Disposition {
            run_id,
            request_id,
            artifact_id,
            digest,
            decision,
        } => (
            "app_run_disposition",
            json!({
                "run_id": run_id,
                "request_id": request_id,
                "artifact_id": artifact_id,
                "digest": digest,
                "decision": decision.as_str(),
            }),
        ),
        RunAction::Ticket { .. } => {
            return Err(Error::internal(
                "the ticket command writes the tracker before run_params",
            ));
        }
    })
}

pub(in super::super) fn run_selected_ticket(
    state_dir: &Path,
    action: &RunAction,
) -> Result<serde_json::Value> {
    let RunAction::Ticket {
        run_id,
        request_id,
        artifact_id,
        digest,
        index,
        project,
    } = action
    else {
        return Err(Error::internal("ticket command expected"));
    };
    if *index == 0 {
        return Err(Error::rejected("suggestion index is 1-based"));
    }
    cadence_agent::issue::model::check_key(project)?;
    let mut reserved = client::rpc(
        state_dir,
        "app_run_ticket_reserve",
        json!({
            "run_id": run_id,
            "request_id": request_id,
            "artifact_id": artifact_id,
            "digest": digest,
            "index": index,
        }),
    )?;
    if reserved
        .get("issue_id")
        .and_then(serde_json::Value::as_str)
        .is_some()
    {
        if let Some(object) = reserved.as_object_mut() {
            object.entry("created").or_insert(json!(false));
        }
        return Ok(reserved);
    }
    let pm = cadence_agent::issue::Pm::open_default()?;
    let cwd = std::env::current_dir()?;
    let created =
        cadence_agent::issue::write::materialize_app_run_ticket(&pm, &cwd, project, &reserved)?;
    let issue_id = created
        .get("id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| Error::internal("ticket issue did not return an id"))?;
    let mut bound = client::rpc(
        state_dir,
        "app_run_ticket_bind",
        json!({
            "run_id": run_id,
            "request_id": request_id,
            "issue_id": issue_id,
        }),
    )?;
    if let Some(object) = bound.as_object_mut() {
        object.insert(
            "created".to_string(),
            created.get("created").cloned().unwrap_or(json!(false)),
        );
    }
    Ok(bound)
}
