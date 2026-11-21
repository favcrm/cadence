//! App run payload helpers, parameter builders, and assistant operation orchestration for `cadence app`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use cadence_agent::client;
use cadence_agent::error::{Error, Result};
use serde_json::{json, Value};

use super::RunAction;

pub(super) fn read_run_inputs(path: &Path) -> Result<serde_json::Value> {
    use std::io::Read;
    const MAX_INPUT_BYTES: usize = 32 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_run_inputs", format!("cannot open inputs: {e}")))?;
    let mut bytes = Vec::new();
    file.take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_run_inputs", format!("cannot read inputs: {e}")))?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(Error::invalid(
            "app_run_inputs",
            "inputs JSON exceeds 32KiB",
        ));
    }
    let inputs: std::collections::BTreeMap<String, String> = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("app_run_inputs", "inputs must be a JSON object of strings"))?;
    Ok(json!(inputs))
}

/// Bounded JSON object for `app assistant invoke`: inline, `@file`, a path,
/// or `-` for stdin. The source never changes the 32 KiB bound.
pub(super) fn parse_assistant_input(state_dir: &Path, raw: &str) -> Result<serde_json::Value> {
    use std::io::Read;
    const MAX_INPUT_BYTES: usize = 32 * 1024;
    let bytes = if raw == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take((MAX_INPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                Error::invalid(
                    "app_assistant_input",
                    format!("cannot read assistant input from stdin: {e}"),
                )
            })?;
        bytes
    } else if raw.trim_start().starts_with('{') {
        raw.as_bytes().to_vec()
    } else {
        let path = Path::new(raw.strip_prefix('@').unwrap_or(raw));
        let file = cadence_agent::master::open_command_file(state_dir, path)?;
        let mut bytes = Vec::new();
        file.take((MAX_INPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                Error::invalid(
                    "app_assistant_input",
                    format!("cannot read assistant input: {e}"),
                )
            })?;
        bytes
    };
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(Error::invalid(
            "app_assistant_input",
            "assistant input JSON exceeds 32KiB",
        ));
    }
    let input: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("app_assistant_input", "input must be a JSON object"))?;
    if !input.is_object() {
        return Err(Error::invalid(
            "app_assistant_input",
            "input must be a JSON object",
        ));
    }
    Ok(input)
}

#[derive(Clone, Debug)]
pub(super) struct AssistantScope {
    pub(super) install_id: String,
    pub(super) context_id: String,
    pub(super) message: String,
    pub(super) token: String,
}

impl AssistantScope {
    /// The daemon's identity fields plus one call's own fields.
    pub(super) fn params(&self, extra: serde_json::Value) -> serde_json::Value {
        let mut params = json!({
            "install_id": self.install_id,
            "context_id": self.context_id,
            "message": self.message,
            "token": self.token,
        });
        if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                params.insert(key.clone(), value.clone());
            }
        }
        params
    }
}

pub(super) fn assistant_scope(
    state_dir: &Path,
    install_id: Option<&str>,
    context_id: Option<&str>,
    message: Option<&str>,
    token: Option<&str>,
) -> Result<AssistantScope> {
    // `agent_identity` derives the caller from the connection, exactly as
    // `cadence self` does. An operator outside a pane has no live turn and
    // must pass every identity flag.
    let live = client::rpc(state_dir, "agent_identity", json!({})).ok();
    resolve_assistant_scope(live.as_ref(), install_id, context_id, message, token)
}

/// Explicit flags win; anything omitted comes from the caller's one live
/// turn and its verified App stamp. Ambiguity refuses instead of guessing.
pub(super) fn resolve_assistant_scope(
    live: Option<&serde_json::Value>,
    install_id: Option<&str>,
    context_id: Option<&str>,
    message: Option<&str>,
    token: Option<&str>,
) -> Result<AssistantScope> {
    let turns = live
        .and_then(|value| value.get("running"))
        .and_then(serde_json::Value::as_array);
    let turn = match (message, token) {
        (Some(id), Some(_)) => {
            turns.and_then(|rows| rows.iter().find(|row| row["id"].as_str() == Some(id)))
        }
        (None, None) => match turns.map(Vec::as_slice) {
            Some([only]) => Some(only),
            Some(rows) if !rows.is_empty() => {
                return Err(Error::rejected(format!(
                    "this agent holds {} running turns — pass --message and --token to choose one",
                    rows.len()
                )))
            }
            _ => None,
        },
        _ => {
            return Err(Error::rejected(
                "--message and --token are given together or not at all",
            ))
        }
    };
    let message =
        match (message, turn.and_then(|row| row["id"].as_str())) {
            (Some(id), _) => id.to_string(),
            (None, Some(id)) => id.to_string(),
            (None, None) if live.is_none() => return Err(Error::rejected(
                "this process sees no live agent turn — pass --message, --token and the install id",
            )),
            (None, None) => {
                return Err(Error::rejected(
                    "no running turn to derive the app scope from — pass --message and --token",
                ))
            }
        };
    let token = match (token, turn.and_then(|row| row["turn_id"].as_str())) {
        (Some(token), _) => token.to_string(),
        (None, Some(token)) => token.to_string(),
        (None, None) => {
            return Err(Error::rejected(
                "no live turn token — pass --token together with --message",
            ))
        }
    };
    let install_id = install_id
        .map(str::to_string)
        .or_else(|| {
            turn.and_then(|row| row["app_install_id"].as_str())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            Error::rejected("this turn carries no App installation — pass the install id")
        })?;
    let context_id = context_id
        .map(str::to_string)
        .or_else(|| {
            turn.and_then(|row| row["app_context_id"].as_str())
                .map(str::to_string)
        })
        .ok_or_else(|| Error::rejected("this turn carries no App context — pass --context-id"))?;
    Ok(AssistantScope {
        install_id,
        context_id,
        message,
        token,
    })
}

/// One idempotency key for one (turn, action, input), so re-running the
/// same command replays the operation instead of creating a second effect.
pub(super) fn derive_operation_id(
    message: &str,
    action_id: &str,
    input: &serde_json::Value,
) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(message.as_bytes());
    digest.update(b"\0");
    digest.update(action_id.as_bytes());
    digest.update(b"\0");
    digest.update(serde_json::to_vec(input).expect("assistant input serializes"));
    let hex = format!("{:x}", digest.finalize());
    format!("op-{}", &hex[..24])
}

pub(super) fn wait_assistant_operation(
    state_dir: &Path,
    scope: &AssistantScope,
    operation_id: &str,
    first: serde_json::Value,
) -> Result<serde_json::Value> {
    use std::time::{Duration, Instant};
    let seconds = std::env::var("CADENCE_ASSISTANT_WAIT_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(120u64);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut current = first;
    loop {
        let status = current
            .get("operation")
            .and_then(|operation| operation.get("status"))
            .or_else(|| current.get("status"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if status != "running" {
            let mut value = current;
            if status == "pending_permission" {
                let revision = value
                    .get("operation")
                    .and_then(|operation| operation.get("revision"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default();
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "wait_note".into(),
                        json!(format!(
                            "operation {operation_id} waits for an operator decision: \
                             `cadence app assistant decide {} --context-id {} \
                             --operation-id {operation_id} --expected-revision {revision} \
                             --decision allow_once` (or `allow_always` / `deny`)",
                            scope.install_id, scope.context_id
                        )),
                    );
                }
            }
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(Error::rejected(format!(
                "operation {operation_id} is still running; read it again with \
                 `cadence app assistant operation-show {} --operation-id {operation_id}`",
                scope.install_id
            )));
        }
        std::thread::sleep(Duration::from_millis(500));
        current = client::rpc(
            state_dir,
            "app_assistant_operation_show",
            scope.params(json!({"operation_id": operation_id})),
        )?;
    }
}

pub(super) fn run_params(action: &RunAction) -> Result<(&'static str, serde_json::Value)> {
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

pub(super) fn run_selected_ticket(
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
