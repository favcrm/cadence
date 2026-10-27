//! The one-shot private SDK child.
//!
//! The trusted broker owns this process completely: a fixed pinned Node
//! executable, a fixed pinned module graph, a cleared environment, `/` as the
//! working directory, private pipes, one bounded request on stdin and one
//! bounded reply on stdout. The provider API key reaches the child only inside
//! that request body — never argv, never the environment, never a file the
//! guest can read, never the daemon log.
//!
//! The child may speak to exactly one approved origin on exactly two fixed
//! POST paths, once each; its own sealed `fetch` wrapper enforces the count,
//! the commitment, the wire bounds, gzip bounds, the frame count and the EOS
//! trailer before returning a reply. The broker still treats the reply as
//! untrusted and re-validates every field, the attempt counts and the
//! usage/EOS evidence. A malformed, truncated, overlong or timeout reply is
//! `unknown` — it retains its reservation; it is never "not dispatched".

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::protected_runtime::strict;

use super::catalog::SdkPins;
use super::dto::{
    ClosedModelTurnErrorV1, NativeAssistantV1, NativeBillingProjectionV1, NativeUsageObservationV1,
};

/// One request body bound: the typed invocation (<=1MiB) plus the plan's
/// pins/catalog and a bounded key.
pub(crate) const MAX_CHILD_REQUEST: usize = 1024 * 1024 + 256 * 1024;
/// One reply bound: the projected assistant (<=1MiB) plus metadata.
pub(crate) const MAX_CHILD_REPLY: usize = 1024 * 1024 + 32 * 1024;
/// Diagnostics are drained and discarded; this only bounds the drain.
const MAX_CHILD_STDERR: u64 = 64 * 1024;
const MAX_NODE_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) struct ChildRequest<'a> {
    pub pins: &'a SdkPins,
    pub plan: &'a Value,
    pub api_key: &'a [u8],
    pub deadline: Duration,
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // one outcome per turn, constructed and matched once
pub(crate) enum ChildOutcome {
    Done {
        assistant: NativeAssistantV1,
        usage: NativeUsageObservationV1,
        auth_requests: u8,
        inference_requests: u8,
    },
    Refused {
        error: ClosedModelTurnErrorV1,
        auth_requests: u8,
        inference_requests: u8,
    },
    Unknown {
        error: ClosedModelTurnErrorV1,
        auth_requests: u8,
        inference_requests: u8,
    },
}

impl ChildOutcome {
    pub(crate) fn billing(&self) -> NativeBillingProjectionV1 {
        match self {
            Self::Done { .. } => NativeBillingProjectionV1::UPSTREAM_OWNED,
            Self::Refused {
                inference_requests, ..
            } => {
                if *inference_requests == 0 {
                    NativeBillingProjectionV1::NOT_DISPATCHED
                } else {
                    NativeBillingProjectionV1::UNKNOWN
                }
            }
            Self::Unknown { .. } => NativeBillingProjectionV1::UNKNOWN,
        }
    }

    pub(crate) fn counts(&self) -> (i64, i64) {
        match self {
            Self::Done {
                auth_requests,
                inference_requests,
                ..
            }
            | Self::Refused {
                auth_requests,
                inference_requests,
                ..
            }
            | Self::Unknown {
                auth_requests,
                inference_requests,
                ..
            } => (*auth_requests as i64, *inference_requests as i64),
        }
    }
}

fn spawn_refused(message: &str) -> Error {
    Error::invalid("route_unavailable", message)
}

fn ambiguous() -> Error {
    Error::invalid(
        "outcome_unknown",
        "the private model child outcome is unknown and remains reserved",
    )
}

/// Verify the pinned executable bytes before it can run.
fn verify_node(pins: &SdkPins) -> Result<()> {
    let meta = std::fs::metadata(&pins.node_path)
        .map_err(|_| spawn_refused("the pinned model runtime is unreadable"))?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_NODE_BYTES {
        return Err(spawn_refused(
            "the pinned model runtime is outside its bound",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o002 != 0 {
            return Err(spawn_refused(
                "the pinned model runtime is writable by other users",
            ));
        }
    }
    let bytes = std::fs::read(&pins.node_path)
        .map_err(|_| spawn_refused("the pinned model runtime is unreadable"))?;
    if cadence_core::connection_dispatch::exact_byte_digest(&bytes) != pins.node_digest {
        return Err(spawn_refused(
            "the pinned model runtime does not match its approved digest",
        ));
    }
    Ok(())
}

fn is_proven_pre_network(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::InvalidInput
            | std::io::ErrorKind::Unsupported
    )
}

#[allow(clippy::disallowed_methods)] // the pinned private model child is launched with a fixed argv
pub(crate) fn run_child(request: ChildRequest<'_>) -> Result<ChildOutcome> {
    verify_node(request.pins)?;
    let plan = serde_json::to_vec(request.plan)
        .map_err(|_| spawn_refused("the model plan could not be encoded"))?;
    if plan.len() > MAX_CHILD_REQUEST / 2 {
        return Err(Error::invalid(
            "payload_too_large",
            "the model plan exceeds its bound",
        ));
    }
    // The key is JSON-escaped into a zeroizing buffer; the buffer is wiped
    // after the single write and never logged.
    let key_text = std::str::from_utf8(request.api_key)
        .map_err(|_| Error::invalid("auth_unavailable", "the model key is unusable"))?;
    let quoted_key = Zeroizing::new(
        serde_json::to_vec(key_text)
            .map_err(|_| spawn_refused("the model key could not be encoded"))?,
    );
    let mut body = Zeroizing::new(Vec::with_capacity(plan.len() + quoted_key.len() + 32));
    body.extend_from_slice(b"{\"v\":1,\"plan\":");
    body.extend_from_slice(&plan);
    body.extend_from_slice(b",\"api_key\":");
    body.extend_from_slice(&quoted_key);
    body.extend_from_slice(b"}\n");
    if body.len() > MAX_CHILD_REQUEST {
        return Err(Error::invalid(
            "payload_too_large",
            "the model invocation exceeds its bound",
        ));
    }

    let mut command = Command::new(&request.pins.node_path);
    command
        // Trust only the pinned Node roots plus the host administrator's
        // system CA store; no caller/environment CA or TLS-disable override.
        .arg("--use-system-ca")
        .arg(&request.pins.child_entry)
        .arg("--pinned-child")
        .current_dir("/")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if is_proven_pre_network(&error) => {
            return Err(spawn_refused("the private model child cannot start"));
        }
        Err(_) => return Err(spawn_refused("the private model child cannot start")),
    };

    let stdin = child.stdin.take();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        if let Some(mut stdin) = stdin {
            stdin.write_all(&body)?;
            stdin.flush()?;
        }
        Ok(())
    });

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        if let Some(stdout) = stdout {
            let mut buffer = Vec::new();
            let mut limited = stdout.take(MAX_CHILD_REPLY as u64 + 1);
            let _ = limited.read_to_end(&mut buffer);
            let _ = tx.send(buffer);
        }
    });
    let stderr = child.stderr.take();
    let drain = std::thread::spawn(move || {
        if let Some(stderr) = stderr {
            let mut sink = Vec::new();
            let _ = stderr.take(MAX_CHILD_STDERR).read_to_end(&mut sink);
            // Diagnostics are discarded: provider error text never survives
            // the private child boundary.
            sink.clear();
        }
    });

    let remaining = request
        .deadline
        .checked_sub(started.elapsed())
        .unwrap_or_else(|| Duration::from_millis(0));
    let reply = rx.recv_timeout(remaining);
    let written = writer.join().unwrap_or(Ok(()));
    let reply = match reply {
        Ok(reply) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let _ = drain.join();
            if written.is_err() {
                return Err(ambiguous());
            }
            reply
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let _ = drain.join();
            return Err(ambiguous());
        }
    };
    if reply.is_empty() || reply.len() > MAX_CHILD_REPLY {
        return Err(ambiguous());
    }
    parse_reply(&reply)
}

fn closed(code: &str) -> ClosedModelTurnErrorV1 {
    match code {
        "auth_unavailable" => ClosedModelTurnErrorV1::auth_unavailable(),
        "route_unavailable" => ClosedModelTurnErrorV1::route_unavailable(),
        "unsupported_transport" => ClosedModelTurnErrorV1::unsupported_transport(),
        "unsupported_content" => ClosedModelTurnErrorV1::unsupported_content(),
        "payload_too_large" => ClosedModelTurnErrorV1::payload_too_large(),
        "invalid_request" => ClosedModelTurnErrorV1::invalid_request(),
        "result_refused" => ClosedModelTurnErrorV1::result_refused(),
        _ => ClosedModelTurnErrorV1::outcome_unknown(),
    }
}

fn parse_reply(raw: &[u8]) -> Result<ChildOutcome> {
    let value = strict::parse(raw).map_err(|_| ambiguous())?;
    let m = strict::object(
        &value,
        &[
            "v",
            "outcome",
            "error",
            "assistant",
            "usage_present",
            "eos",
            "auth_requests",
            "inference_requests",
        ],
    )
    .map_err(|_| ambiguous())?;
    if strict::safe_u64(m, "v").map_err(|_| ambiguous())? != 1 {
        return Err(ambiguous());
    }
    let auth_requests = strict::safe_u64(m, "auth_requests").map_err(|_| ambiguous())?;
    let inference_requests = strict::safe_u64(m, "inference_requests").map_err(|_| ambiguous())?;
    if auth_requests > 1 || inference_requests > 1 {
        return Err(ambiguous());
    }
    let usage_present = m
        .get("usage_present")
        .and_then(Value::as_bool)
        .ok_or_else(ambiguous)?;
    let eos = m
        .get("eos")
        .and_then(Value::as_bool)
        .ok_or_else(ambiguous)?;
    let error = match m.get("error") {
        Some(Value::Null) => None,
        Some(value) => {
            let e = strict::object(value, &["code"]).map_err(|_| ambiguous())?;
            Some(closed(
                e.get("code")
                    .and_then(Value::as_str)
                    .ok_or_else(ambiguous)?,
            ))
        }
        None => return Err(ambiguous()),
    };
    match strict::string(m, "outcome").map_err(|_| ambiguous())? {
        "done" => {
            let assistant = m.get("assistant").ok_or_else(ambiguous)?;
            if assistant.is_null() {
                return Err(ambiguous());
            }
            if inference_requests != 1 || !eos || !usage_present || error.is_some() {
                return Err(ambiguous());
            }
            let assistant = NativeAssistantV1::parse(assistant)?;
            let usage = NativeUsageObservationV1::from_assistant(&assistant).ok_or_else(|| {
                Error::invalid("outcome_unknown", "the usage observation is inconsistent")
            })?;
            Ok(ChildOutcome::Done {
                assistant,
                usage,
                auth_requests: auth_requests as u8,
                inference_requests: inference_requests as u8,
            })
        }
        "refused" => {
            if inference_requests != 0 {
                return Err(ambiguous());
            }
            let error = error.unwrap_or_else(ClosedModelTurnErrorV1::result_refused);
            Ok(ChildOutcome::Refused {
                error,
                auth_requests: auth_requests as u8,
                inference_requests: 0,
            })
        }
        "unknown" => Ok(ChildOutcome::Unknown {
            error: error.unwrap_or_else(ClosedModelTurnErrorV1::outcome_unknown),
            auth_requests: auth_requests as u8,
            inference_requests: inference_requests as u8,
        }),
        _ => Err(ambiguous()),
    }
}

/// The child plan. It carries no secret: the key is written separately in the
/// same request body. Every field is server-derived.
#[allow(clippy::too_many_arguments)]
pub(crate) fn child_plan(
    pins: &SdkPins,
    provider: &str,
    origin: &str,
    configured_model: &str,
    model_id: &str,
    model_uid: &str,
    reasoning: bool,
    effort: &str,
    thinking_level_map: &Value,
    max_output_tokens: u32,
    tool_choice: &str,
    messages: &Value,
    tools: &Value,
    commitment: &str,
    input_digest: &str,
    sdk_graph_digest: &str,
    source_id: &str,
    account: &str,
    source_revision: u64,
    configuration_id: &str,
    configuration_revision: u64,
    snapshot_id: &str,
    loaded_receipt_id: &str,
    runtime_generation: u64,
    owner_epoch: u64,
) -> Value {
    json!({
        "v": 1,
        "pins": pins.to_plan_json(),
        "route": {
            "provider": provider,
            "origin": origin,
            "configured_model": configured_model,
            "model_id": model_id,
            "model_uid": model_uid,
            "reasoning": reasoning,
            "effort": effort,
            "thinking_level_map": thinking_level_map,
            "max_output_tokens": max_output_tokens,
            "tool_choice": tool_choice,
            "paths": [
                "/exa.auth_pb.AuthService/GetUserJwt",
                "/exa.api_server_pb.ApiServerService/GetChatMessage"
            ],
            "commitment": commitment,
            "input_digest": input_digest,
            "sdk_graph_digest": sdk_graph_digest,
            "source_id": source_id,
            "account": account,
            "source_revision": source_revision,
            "configuration_id": configuration_id,
            "configuration_revision": configuration_revision,
            "snapshot_id": snapshot_id,
            "loaded_receipt_id": loaded_receipt_id,
            "runtime_generation": runtime_generation,
            "owner_epoch": owner_epoch,
        },
        "messages": messages,
        "tools": tools,
    })
}
