//! The published `hosted-cadence-native-agent.v1` transport.
//!
//! One endpoint on the tenant board origin: `POST /__platform/native-agent/call`
//! with the existing `hct_` hosted-cadence credential. Actions are
//! `agent`/`submit`/`readback`/`cancel`; every call revalidates live
//! credential, membership and scope server-side, so there is no local
//! authorize envelope and no cookie/query-token fallback.
//!
//! Published by the AgenticOS companion (AOS-395). The interface is frozen at
//! revision 3: native consumes the exact shapes below, requires the stable
//! `invoker_id` owner proof, and refuses unknown/mismatched fields instead of
//! guessing. This module is the consumer mapping only; it widens no native
//! authority.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::remote_cli::RemoteTarget;

use super::{
    CloudTransport, RemoteAgent, RemoteOperation, RemoteStatus, Source, SubmitRequest,
    OPERATION_CAP, REQUEST_CAP, SCOPE_READ, SCOPE_WRITE,
};

pub const CONTRACT_VERSION: &str = "hosted-cadence-native-agent.v1";
pub const CONTRACT_PATH: &str = "/__platform/native-agent/call";

/// One published call. Only the tenant board origin and this exact path are
/// reachable; the saved credential is never returned to a caller.
fn call_for(
    target: &RemoteTarget,
    source: Option<&Source>,
    action: &str,
    arguments: Value,
) -> Result<Value> {
    if source.is_some_and(|source| {
        !matches!(
            source.kind,
            crate::store::cloud_agent::SOURCE_USER | crate::store::cloud_agent::SOURCE_PEER
        )
    }) {
        return Err(Error::gate_coded(
            "cloud_agent_source_refused",
            "unknown cloud-agent source; no credential was selected",
        ));
    }
    let body = json!({
        "version": CONTRACT_VERSION,
        "action": action,
        "arguments": arguments,
    });
    let scope = match action {
        "submit" | "cancel" => SCOPE_WRITE,
        _ => SCOPE_READ,
    };
    let (status, body) = match source {
        Some(source) if source.kind == crate::store::cloud_agent::SOURCE_PEER => {
            crate::remote_cli::cloud_agent_peer_post(
                target,
                &source.owner,
                &source.uid,
                source.epoch,
                CONTRACT_PATH,
                &body,
                scope,
            )?
        }
        _ => crate::remote_cli::cloud_agent_post(target, CONTRACT_PATH, &body, scope)?,
    };
    if (200..300).contains(&status) {
        let answer = body.ok_or_else(|| {
            Error::unknown_coded(
                "cloud_agent_uncertain",
                "cloud-agent contract answered without a JSON body",
            )
        })?;
        if answer["ok"].as_bool() != Some(true) {
            return Err(Error::unknown_coded(
                "cloud_agent_uncertain",
                "cloud-agent contract answered without ok:true",
            ));
        }
        return Ok(answer);
    }
    Err(decode(status, body.as_ref()))
}

fn wire_code(body: Option<&Value>) -> Option<&str> {
    body.and_then(|b| b["error"]["code"].as_str())
        .or_else(|| body.and_then(|b| b["code"].as_str()))
}

fn decode(status: u16, body: Option<&Value>) -> Error {
    let code = wire_code(body);
    let message = body
        .and_then(|b| b["error"]["message"].as_str())
        .unwrap_or("the cloud-agent contract refused the request");
    match (status, code) {
        (400, _) | (_, Some("invalid_request")) => {
            Error::invalid("cloud_agent_invalid_request", message)
        }
        (401, _) | (_, Some("unauthorized")) => {
            Error::gate_coded("cloud_agent_unauthorized", message)
        }
        (403, _) | (_, Some("forbidden")) => {
            Error::gate_coded("cloud_agent_scope_revoked", message)
        }
        // The contract decides both 409 cases BEFORE any model enqueue:
        // `session_busy` means nothing was written, `operation_conflict`
        // means a different payload already owns this id. Neither is an
        // uncertain accepted turn, so neither may be read back/replayed.
        (409, Some("session_busy")) => Error::conflict_coded(
            "cloud_agent_session_busy",
            "another turn on this provider session is still unsettled; nothing was sent",
        ),
        (409, _) | (_, Some("operation_conflict")) => Error::conflict_coded(
            "cloud_agent_replay_conflict",
            "operation id was already used with a different request; nothing was invoked",
        ),
        (501, Some("cancel_unsupported")) => Error::gate_coded(
            "cloud_agent_cancel_unsupported",
            "cancellation is unsupported by the cloud-agent contract; nothing is stopped",
        ),
        (501, _) => Error::invalid("cloud_agent_invalid_request", message),
        // 503 can be returned AFTER the durable enqueue committed (the
        // acknowledgement was lost), so this is uncertain, never a known
        // refusal: the operation stays non-terminal and only readback may
        // settle it. It must never trigger a second submit.
        (503, _) | (_, Some("capability_unavailable")) => Error::unknown_coded(
            "cloud_agent_unavailable",
            "the cloud runtime's private control binding is unavailable; the request may \
             already be accepted — read the original operation back, do not resend it",
        ),
        (500..=599, _) => Error::unknown_coded(
            "cloud_agent_uncertain",
            "the cloud-agent contract failed after the request may have been accepted",
        ),
        _ => Error::unknown_coded(
            "cloud_agent_uncertain",
            "the cloud-agent contract answered with an unrecognised refusal",
        ),
    }
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value.get(name).and_then(Value::as_str).ok_or_else(|| {
        Error::unknown_coded(
            "cloud_agent_uncertain",
            format!("contract field {name} is missing"),
        )
    })
}

fn optional_text(value: &Value, name: &str) -> Option<String> {
    value.get(name).and_then(Value::as_str).map(str::to_string)
}

fn take_optional_text(value: &mut Value, name: &str) -> Option<String> {
    match value.get_mut(name).map(Value::take) {
        Some(Value::String(text)) => Some(text),
        _ => None,
    }
}

fn take_text(value: &mut Value, name: &str) -> Result<String> {
    take_optional_text(value, name).ok_or_else(|| {
        Error::unknown_coded(
            "cloud_agent_uncertain",
            format!("contract field {name} is missing"),
        )
    })
}

fn take_object(value: &mut Value, name: &str) -> Result<Value> {
    match value.get_mut(name) {
        Some(value) if value.is_object() => Ok(value.take()),
        _ => Err(Error::unknown_coded(
            "cloud_agent_uncertain",
            format!("contract field {name} is missing"),
        )),
    }
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Value> {
    value.get(name).filter(|v| v.is_object()).ok_or_else(|| {
        Error::unknown_coded(
            "cloud_agent_uncertain",
            format!("contract field {name} is missing"),
        )
    })
}

/// The contract's stable, non-secret logical-owner id (`inv_` + 32 chars).
/// Requiring it turns a v2/echo-only answer into an uncertain result instead
/// of silently accepting a reply with no stable owner proof.
fn invoker_id(provenance: &Value) -> Result<&str> {
    let id = text(provenance, "invoker_id")?;
    let valid = id.len() == 36
        && id.starts_with("inv_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if !valid {
        return Err(Error::unknown_coded(
            "cloud_agent_uncertain",
            "the contract returned a malformed stable owner id (invoker_id)",
        ));
    }
    Ok(id)
}

/// The contract's answer must describe the exact invoker this caller proved:
/// the same kind, and for a peer the same native UID, registration stamp and
/// cloud-purpose role. Anything else is a refused/uncertain answer, never a
/// reply to adopt.
fn check_provenance(provenance: &Value, source: &Source) -> Result<()> {
    if provenance["kind"].as_str() != Some(source.kind) {
        return Err(Error::unknown_coded(
            "cloud_agent_uncertain",
            "the contract derived a different principal than the verified invoker",
        ));
    }
    if source.kind == crate::store::cloud_agent::SOURCE_PEER {
        let peer = &provenance["peer"];
        let sender = &peer["sender"];
        if peer["role"].as_str() != Some("cloud_agent")
            || sender["uid"].as_str() != Some(source.uid.as_str())
            || sender["registration_epoch"].as_i64() != Some(source.epoch)
        {
            return Err(Error::gate_coded(
                "cloud_agent_peer_mismatch",
                "the contract's peer provenance does not name this agent's immutable \
                 sender identity; no reply was adopted",
            ));
        }
    } else if !provenance["peer"].is_null() {
        return Err(Error::unknown_coded(
            "cloud_agent_uncertain",
            "the contract attached peer provenance to a user invocation",
        ));
    }
    Ok(())
}

fn status_of(value: &str) -> Result<RemoteStatus> {
    // Rev2: submit and readback share one status union.
    Ok(match value {
        "queued" => RemoteStatus::Queued,
        "running" => RemoteStatus::Running,
        "parked" => RemoteStatus::Parked,
        "completed" => RemoteStatus::Completed,
        "failed" => RemoteStatus::Failed,
        "unknown" => RemoteStatus::Unknown,
        other => {
            return Err(Error::unknown_coded(
                "cloud_agent_uncertain",
                format!("contract returned an unrecognised operation status '{other}'"),
            ))
        }
    })
}

/// Validate the echoed correlation values: the server must answer about the
/// exact conversation/operation this caller supplied.
fn check_echo(operation: &Value, conversation_id: &str, operation_id: &str) -> Result<()> {
    if text(operation, "operation_id")? != operation_id
        || text(operation, "conversation_id")? != conversation_id
    {
        return Err(Error::unknown_coded(
            "cloud_agent_uncertain",
            "contract answered for a different conversation or operation",
        ));
    }
    Ok(())
}

fn operation_from(
    answer: &Value,
    conversation_id: &str,
    operation_id: &str,
) -> Result<RemoteOperation> {
    let operation = object(answer, "operation")?;
    check_echo(operation, conversation_id, operation_id)?;
    let status = status_of(text(operation, "status")?)?;
    let reply_text = optional_text(operation, "text");
    let reason = optional_text(operation, "reason");
    let error = match status {
        RemoteStatus::Failed => Some((
            "cloud_agent_remote_failed".to_string(),
            reason.unwrap_or_else(|| "the cloud runtime reported the turn failed".to_string()),
        )),
        _ => None,
    };
    Ok(RemoteOperation {
        status,
        remote_operation_id: Some(operation_id.to_string()),
        remote_session_id: None,
        provider_session_id: None,
        invoker_id: None,
        reply: (status == RemoteStatus::Completed)
            .then_some(reply_text)
            .flatten(),
        error,
    })
}

pub struct HostedCloudTransport;

impl CloudTransport for HostedCloudTransport {
    fn connect(
        &self,
        target: &RemoteTarget,
        conversation_id: &str,
        source: Option<&Source>,
    ) -> Result<RemoteAgent> {
        let mut answer = call_for(
            target,
            source,
            "agent",
            json!({"conversation_id": conversation_id}),
        )?;
        let mut agent = take_object(&mut answer, "agent")?;
        let mut conversation = take_object(&mut answer, "conversation")?;
        let capabilities = take_object(&mut answer, "capabilities")?;
        let mut runtime = take_object(&mut answer, "runtime")?;
        let provenance = take_object(&mut agent, "provenance")?;
        let invoker_id = invoker_id(&provenance)?.to_string();
        if take_text(&mut conversation, "conversation_id")? != conversation_id {
            return Err(Error::gate_coded(
                "cloud_agent_session_mismatch",
                "discovery answered for a different work conversation",
            ));
        }
        let remote_session_id = take_text(&mut conversation, "remote_session_id")?;
        if remote_session_id.is_empty() {
            return Err(Error::unknown_coded(
                "cloud_agent_uncertain",
                "discovery returned no provider session binding",
            ));
        }
        Ok(RemoteAgent {
            remote_agent_id: take_text(&mut agent, "agent_id")?,
            organization_id: take_text(&mut agent, "organization_id")?,
            audience: take_text(&mut agent, "audience")?,
            provenance,
            invoker_id,
            remote_session_id,
            provider_session_id: take_optional_text(&mut conversation, "provider_session_id"),
            capabilities,
            runtime: take_text(&mut runtime, "kind")?,
            model_label: take_optional_text(&mut runtime, "model_label"),
        })
    }

    fn submit(
        &self,
        target: &RemoteTarget,
        request: &SubmitRequest<'_>,
    ) -> Result<RemoteOperation> {
        let answer = call_for(
            target,
            Some(request.source),
            "submit",
            json!({
                "conversation_id": request.conversation_id,
                "operation_id": request.operation_id,
                "text": request.text,
            }),
        )?;
        let mut operation = operation_from(&answer, request.conversation_id, request.operation_id)?;
        let provenance = object(object(&answer, "agent")?, "provenance")?;
        check_provenance(provenance, request.source)?;
        operation.invoker_id = Some(invoker_id(provenance)?.to_string());
        if let Some(conversation) = answer.get("conversation").filter(|v| v.is_object()) {
            operation.remote_session_id = conversation
                .get("remote_session_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            operation.provider_session_id = optional_text(conversation, "provider_session_id");
        }
        // `accepted:false` means this exact operation+payload was already
        // accepted: no second model call. The status/reply is authoritative.
        match answer["operation"]["accepted"].as_bool() {
            Some(_) => {}
            None => {
                return Err(Error::unknown_coded(
                    "cloud_agent_uncertain",
                    "contract submit answer did not state whether it accepted the turn",
                ))
            }
        }
        Ok(operation)
    }

    fn read(
        &self,
        target: &RemoteTarget,
        conversation_id: &str,
        operation_id: &str,
        source: &Source,
    ) -> Result<RemoteOperation> {
        let answer = call_for(
            target,
            Some(source),
            "readback",
            json!({"conversation_id": conversation_id, "operation_id": operation_id}),
        )?;
        operation_from(&answer, conversation_id, operation_id)
    }

    fn cancel(
        &self,
        target: &RemoteTarget,
        conversation_id: &str,
        operation_id: &str,
        source: &Source,
    ) -> Result<()> {
        call_for(
            target,
            Some(source),
            "cancel",
            json!({"conversation_id": conversation_id, "operation_id": operation_id}),
        )
        .map(|_| ())
    }
}

// The contract limits mirrored here so a local refusal happens before a
// request that the server would reject anyway.
const _: () = {
    assert!(OPERATION_CAP >= 8);
    assert!(REQUEST_CAP > 0);
};
