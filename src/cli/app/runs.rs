//! App run payload helpers, parameter builders, and assistant operation orchestration for `cadence app`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

mod params;

use std::path::Path;

use cadence_agent::client;
use cadence_agent::error::{Error, Result};
use serde_json::json;

pub(super) use params::{run_params, run_selected_ticket};

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
