//! One bounded, private call to an owner-selected AgenticOS inference gateway.
use std::io::Read;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::dto::{
    self, AssistantPartV1, ClosedModelTurnErrorV1, NativeAssistantV1, NativeMessageV1,
    NativeToolV1, NativeUsageObservationV1, TextPartV1,
};
use super::sdk::{ChildOutcome, MAX_CHILD_REPLY};
use crate::error::Error;
use crate::protected_runtime::strict;
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};

pub(super) const CHAT_PATH: &str = "/v1/runtime/inference/chat/completions";
// AgenticOS's text-context budget; this broker never sends image data.
const MAX_GATEWAY_REQUEST: usize = 256 * 1024;

fn unsupported() -> Error {
    Error::invalid("unsupported_content", "the model content is not supported")
}

const PRODUCTION_ORIGIN: &str = "https://api-v2.agenticos.hk";
static PRODUCTION_TLS: LazyLock<TlsConfig> = LazyLock::new(|| {
    TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::WebPki)
        .build()
});

#[derive(Clone, Debug)]
pub(super) enum GatewayTls {
    Loopback(TlsConfig),
    Production,
}

impl GatewayTls {
    pub(super) fn origin<'a>(&self, value: &'a str) -> crate::Result<&'a str> {
        let invalid = || Error::invalid("route_unavailable", "the gateway origin is invalid");
        match self {
            Self::Production if value == PRODUCTION_ORIGIN => Ok(value),
            Self::Loopback(_) => loopback_origin(value),
            _ => Err(invalid()),
        }
    }

    fn config(&self) -> TlsConfig {
        match self {
            Self::Loopback(tls) => tls.clone(),
            Self::Production => PRODUCTION_TLS.clone(),
        }
    }
}

fn loopback_origin(value: &str) -> crate::Result<&str> {
    let invalid = || Error::invalid("route_unavailable", "the gateway origin is invalid");
    let port = value
        .strip_prefix("https://127.0.0.1:")
        .ok_or_else(invalid)?;
    if port.starts_with('0')
        || !port.bytes().all(|b| b.is_ascii_digit())
        || port.parse::<u16>().is_err()
    {
        return Err(invalid());
    }
    Ok(value)
}

pub(super) fn pinned_tls(pem: &str) -> crate::Result<GatewayTls> {
    // Loopback alone proves neither listener ownership nor server identity.
    // Trust only this operator-provisioned root and require the IP SAN.
    let cert = Certificate::from_pem(pem.as_bytes())
        .map_err(|_| Error::invalid("route_unavailable", "the gateway TLS root is invalid"))?;
    Ok(GatewayTls::Loopback(
        TlsConfig::builder()
            .root_certs(RootCerts::new_with_certs(&[cert]))
            .build(),
    ))
}

pub(super) struct GatewayTurn<'a> {
    pub origin: &'a str,
    pub tls: &'a GatewayTls,
    pub configured_model: &'a str,
    pub model_id: &'a str,
    pub effort: &'a str,
    pub request_id: &'a str,
    pub workspace_id: &'a str,
    pub max_output_tokens: u32,
    pub tool_choice: &'a str,
    pub messages: &'a [NativeMessageV1],
    pub tools: &'a [NativeToolV1],
    pub bearer: &'a [u8],
    pub deadline: Duration,
}

fn text(parts: &[TextPartV1]) -> crate::Result<String> {
    let mut result = String::new();
    for part in parts {
        if part.signature.is_some() {
            return Err(unsupported());
        }
        result.push_str(&part.text);
    }
    Ok(result)
}

/// AgenticOS's OpenAI-compatible gateway does not enforce constrained
/// sampling (`strict` is ignored on the gateway). Match Pi 1.4's
/// `resolveJsonSchemaStrictSampling`: a best-effort `json_schema` hint
/// (`strict: "prefer"`) degrades to a plain function tool declaration, while
/// `strict: "require"`, `grammar` constraints, or unknown shapes fail closed.
fn admits_tool_constrained_sampling(value: Option<&Value>) -> bool {
    match value {
        None => true,
        Some(Value::Object(map)) => {
            map.len() == 2
                && map.get("type").and_then(Value::as_str) == Some("json_schema")
                && map.get("strict").and_then(Value::as_str) == Some("prefer")
        }
        Some(_) => false,
    }
}

fn request(turn: &GatewayTurn<'_>) -> crate::Result<Vec<u8>> {
    if turn.messages.is_empty()
        || turn.max_output_tokens == 0
        || !matches!(turn.tool_choice, "auto" | "none")
    {
        return Err(unsupported());
    }
    let mut messages = Vec::with_capacity(turn.messages.len());
    for message in turn.messages {
        messages.push(match message {
            NativeMessageV1::System { content, .. } => {
                json!({"role":"system", "content":text(content)?})
            }
            NativeMessageV1::User { content, .. } => {
                json!({"role":"user", "content":text(content)?})
            }
            NativeMessageV1::ToolResult {
                tool_call_id,
                content,
                ..
            } => json!({"role":"tool", "tool_call_id":tool_call_id, "content":text(content)?}),
            NativeMessageV1::Assistant(history) => {
                let mut content = String::new();
                let mut calls = Vec::new();
                for part in &history.content {
                    match part {
                        AssistantPartV1::Text(part) => {
                            if part.signature.is_some() {
                                return Err(unsupported());
                            }
                            content.push_str(&part.text);
                        }
                        AssistantPartV1::ToolCall(call) => {
                            if call.thought_signature.is_some()
                                || call.namespace.is_some()
                                || call.arguments_error.is_some()
                                || !call.arguments.is_object()
                            {
                                return Err(unsupported());
                            }
                            let args = serde_json::to_string(&call.arguments)
                                .map_err(|_| unsupported())?;
                            calls.push(json!({
                                "id": call.id,
                                "type": "function",
                                "function": {"name": call.name, "arguments": args}
                            }));
                        }
                        AssistantPartV1::Thinking(_) => return Err(unsupported()),
                    }
                }
                json!({"role":"assistant", "content":content, "tool_calls":calls})
            }
        });
    }
    // AgenticOS ignores tool_choice; do not expose declarations on a no-tool turn.
    let declared = if turn.tool_choice == "auto" {
        turn.tools
    } else {
        &[]
    };
    let mut tools = Vec::with_capacity(declared.len());
    for tool in declared {
        if !admits_tool_constrained_sampling(tool.constrained_sampling.as_ref())
            || !tool.parameters.is_object()
        {
            return Err(unsupported());
        }
        tools.push(json!({
            "type": "function",
            "function": {"name": tool.name, "description": tool.description, "parameters": tool.parameters}
        }));
    }
    #[derive(serde::Serialize)]
    struct Reasoning {
        effort: &'static str,
    }

    #[derive(serde::Serialize)]
    struct ChatRequest<'a> {
        model: &'a str,
        messages: Vec<Value>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tools: Vec<Value>,
        max_tokens: u32,
        stream: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning: Option<Reasoning>,
    }
    let body = serde_json::to_vec(&ChatRequest {
        model: turn.model_id,
        messages,
        tools,
        max_tokens: turn.max_output_tokens,
        stream: false,
        reasoning: match (turn.model_id, turn.effort) {
            (_, "off") => None,
            ("tencent/hy4", "high") => Some(Reasoning { effort: "high" }),
            _ => return Err(unsupported()),
        },
    })
    .map_err(|_| unsupported())?;
    if body.len() > MAX_GATEWAY_REQUEST {
        return Err(Error::invalid(
            "payload_too_large",
            "the model request exceeds its bound",
        ));
    }
    Ok(body)
}

fn unknown() -> ChildOutcome {
    ChildOutcome::Unknown {
        error: ClosedModelTurnErrorV1::outcome_unknown(),
        auth_requests: 0,
        inference_requests: 1,
    }
}

fn nullable_count(value: &Value) -> Option<Option<u64>> {
    match value {
        Value::Null => Some(None),
        other => other.as_u64().map(Some),
    }
}

fn response(value: &Value, turn: &GatewayTurn<'_>) -> Option<ChildOutcome> {
    if value.get("object")?.as_str()? != "chat.completion" {
        return None;
    }
    let model = value.get("model")?.as_str()?;
    if model != turn.model_id {
        return None;
    }
    let choices = value.get("choices")?.as_array()?;
    if choices.len() != 1 || choices[0].get("index")?.as_u64()? != 0 {
        return None;
    }
    let choice = &choices[0];
    let message = choice.get("message")?;
    if message.get("role")?.as_str()? != "assistant" {
        return None;
    }
    let mut parts = Vec::new();
    match message.get("content")? {
        Value::String(text) => parts.push(json!({"type":"text","text":text})),
        Value::Null => (),
        _ => return None,
    }
    if let Some(calls) = message.get("tool_calls") {
        let calls = calls.as_array()?;
        if calls.len() > dto::MAX_TOOL_CALLS || (turn.tool_choice == "none" && !calls.is_empty()) {
            return None;
        }
        for call in calls {
            if call.get("type")?.as_str()? != "function" {
                return None;
            }
            let function = call.get("function")?;
            let arguments = strict::parse(function.get("arguments")?.as_str()?.as_bytes()).ok()?;
            if !arguments.is_object() {
                return None;
            }
            dto::validate_integer_json(&arguments, 0).ok()?;
            parts.push(json!({
                "type": "toolCall",
                "id": call.get("id")?.as_str()?,
                "name": function.get("name")?.as_str()?,
                "arguments": arguments,
            }));
        }
    }
    let stop = match choice.get("finish_reason")?.as_str()? {
        "stop" => "stop",
        "length" => "length",
        "tool_calls" => "toolUse",
        _ => return None,
    };
    let usage = value.get("usage")?;
    let input = nullable_count(usage.get("prompt_tokens")?)?;
    let output = nullable_count(usage.get("completion_tokens")?)?;
    let total = nullable_count(usage.get("total_tokens")?)?;
    let cached = match usage.get("prompt_tokens_details") {
        None | Some(Value::Null) => 0,
        Some(details) => details.get("cached_tokens")?.as_u64()?,
    };
    if input.is_some_and(|input| cached > input) {
        return None;
    }
    if let Some(details) = usage.get("completion_tokens_details") {
        if !details.is_null() {
            let reasoning = details.get("reasoning_tokens")?.as_u64()?;
            if output.is_some_and(|output| reasoning > output) {
                return None;
            }
        }
    }
    let observed = match (input, output, total) {
        (Some(input), Some(output), Some(total)) => {
            if input.checked_add(output)? != total {
                return None;
            }
            Some((input - cached, output, cached, total))
        }
        _ => None,
    };
    // Pi requires numeric usage on assistant history. This placeholder
    // never becomes a usage observation or a local billing decision.
    let (input, output, cached, total) = observed.unwrap_or_default();
    let timestamp: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()?;
    let mut assistant = json!({
        "role": "assistant",
        "content": parts,
        "api": "agenticos",
        "provider": "agenticos",
        "model": turn.configured_model,
        "responseModel": model,
        "usage": {
            "input": input, "output": output, "cacheRead": cached, "cacheWrite": 0,
            "totalTokens": total, "cost": {"input":0, "output":0, "cacheRead":0, "cacheWrite":0, "total":0},
        },
        "stopReason": stop,
        "timestamp": timestamp,
    });
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        if id.is_empty() || id.len() > 256 {
            return None;
        }
        assistant["responseId"] = json!(id);
    }
    let assistant = NativeAssistantV1::parse(&assistant).ok()?;
    let usage = match observed {
        Some(_) => Some(NativeUsageObservationV1::from_assistant(&assistant)?),
        None => None,
    };
    Some(ChildOutcome::Done {
        assistant,
        usage,
        auth_requests: 0,
        inference_requests: 1,
    })
}

pub(super) fn invoke(turn: GatewayTurn<'_>) -> crate::Result<ChildOutcome> {
    let origin = turn.tls.origin(turn.origin)?;
    let token = std::str::from_utf8(turn.bearer)
        .map_err(|_| Error::invalid("auth_unavailable", "the gateway credential is unusable"))?;
    if token.is_empty() || !token.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(Error::invalid(
            "auth_unavailable",
            "the gateway credential is unusable",
        ));
    }
    let body = request(&turn)?;
    let mut hash = Sha256::new();
    hash.update(b"cadence.agenticos.gateway.idempotency.v1\0");
    hash.update(turn.request_id.as_bytes());
    let key = format!("cadence-{:x}", hash.finalize());
    let authorization = Zeroizing::new(format!("Bearer {token}"));
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(turn.deadline))
            .https_only(true)
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .tls_config(turn.tls.config())
            .build(),
    );
    let result = agent
        .post(format!("{origin}{CHAT_PATH}"))
        .header("Content-Type", "application/json")
        .header("Authorization", authorization.as_str())
        .header("Idempotency-Key", &key)
        .header("x-agenticos-workspace", turn.workspace_id)
        .send(body.as_slice());
    let Ok(reply) = result else {
        return Ok(unknown());
    };
    if reply.status().as_u16() != 200 {
        return Ok(unknown());
    }
    let mut bytes = Vec::new();
    if reply
        .into_body()
        .as_reader()
        .take(MAX_CHILD_REPLY as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_CHILD_REPLY
    {
        return Ok(unknown());
    }
    Ok(strict::parse(&bytes)
        .ok()
        .and_then(|value| response(&value, &turn))
        .unwrap_or_else(unknown))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn fixture_turn<'a>(tls: &'a GatewayTls, messages: &'a [NativeMessageV1]) -> GatewayTurn<'a> {
        GatewayTurn {
            origin: PRODUCTION_ORIGIN,
            tls,
            configured_model: "agenticos/example/model",
            model_id: "example/model",
            effort: "off",
            request_id: "synthetic-request",
            workspace_id: "ws_synthetic",
            max_output_tokens: 64,
            tool_choice: "none",
            messages,
            tools: &[],
            bearer: b"synthetic-bearer",
            deadline: Duration::from_secs(1),
        }
    }

    fn user_message(text: String) -> NativeMessageV1 {
        NativeMessageV1::User {
            content: vec![TextPartV1 {
                text,
                signature: None,
            }],
            timestamp: 1,
        }
    }

    #[test]
    fn gateway_omits_empty_declarations_and_refuses_oversize_without_dispatch() {
        let small = [user_message("Reply plainly".into())];
        let turn = fixture_turn(&GatewayTls::Production, &small);
        let wire: Value = serde_json::from_slice(&request(&turn).unwrap()).unwrap();
        assert_eq!(wire["messages"][0]["content"], "Reply plainly");
        assert!(wire.get("tools").is_none());
        assert!(wire.get("reasoning").is_none());
        let mut high = fixture_turn(&GatewayTls::Production, &small);
        high.model_id = "tencent/hy4";
        high.effort = "high";
        let wire: Value = serde_json::from_slice(&request(&high).unwrap()).unwrap();
        assert_eq!(wire["reasoning"], json!({"effort": "high"}));

        let large: Vec<_> = (0..5).map(|_| user_message("x".repeat(60_000))).collect();
        let error = request(&fixture_turn(&GatewayTls::Production, &large)).unwrap_err();
        assert_eq!(error.code(), Some("payload_too_large"));
    }

    #[test]
    fn gateway_delivers_completion_without_fabricating_metered_usage() {
        let turn = fixture_turn(&GatewayTls::Production, &[]);
        let mut wire = json!({
            "object":"chat.completion",
            "model":"example/model",
            "choices":[{
                "index":0,
                "message":{"role":"assistant","content":"The answer"},
                "finish_reason":"stop"
            }],
            "usage":{"prompt_tokens":12,"completion_tokens":null,"total_tokens":null},
        });
        let Some(ChildOutcome::Done {
            assistant, usage, ..
        }) = response(&wire, &turn)
        else {
            panic!("a successful reply with unavailable usage was discarded");
        };
        assert_eq!(assistant.to_json()["content"][0]["text"], "The answer");
        assert!(usage.is_none());

        wire["usage"] = json!({"prompt_tokens":12,"completion_tokens":3,"total_tokens":15});
        let Some(ChildOutcome::Done {
            usage: Some(usage), ..
        }) = response(&wire, &turn)
        else {
            panic!("valid observed usage was not preserved");
        };
        assert_eq!((usage.input, usage.output, usage.total), (12, 3, 15));

        wire["usage"]["total_tokens"] = json!(16);
        assert!(response(&wire, &turn).is_none());
    }

    #[test]
    fn transport_binds_origin_to_owner_tls_mode_before_sending_credentials() {
        let production = GatewayTls::Production;
        let public_tls = production.config();
        assert!(matches!(public_tls.provider(), TlsProvider::Rustls));
        assert!(matches!(public_tls.root_certs(), RootCerts::WebPki));
        assert!(!public_tls.disable_verification());
        assert!(public_tls.use_sni());
        let loopback = GatewayTls::Loopback(
            TlsConfig::builder()
                .root_certs(RootCerts::new_with_certs(&[]))
                .build(),
        );
        for (tls, origin) in [
            (&production, "https://127.0.0.1:8443"),
            (&loopback, "https://api-v2.agenticos.hk"),
            (&production, "https://api-v2.agenticos.hk:443"),
            (&production, "https://api-v2.agenticos.hk@evil.test"),
        ] {
            let result = invoke(GatewayTurn {
                origin,
                tls,
                configured_model: "agenticos/example/model",
                model_id: "example/model",
                effort: "off",
                request_id: "synthetic-request",
                workspace_id: "ws_synthetic",
                max_output_tokens: 64,
                tool_choice: "none",
                messages: &[],
                tools: &[],
                bearer: b"synthetic-bearer",
                deadline: Duration::from_secs(1),
            });
            let Err(error) = result else {
                panic!("unexpectedly accepted {origin}");
            };
            assert_eq!(error.code(), Some("route_unavailable"), "{origin}");
        }
    }

    #[test]
    fn loopback_listener_cannot_receive_the_bearer_without_tls() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let origin = format!("https://{}", listener.local_addr().unwrap());
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let peer = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut record_header = [0; 5];
            connection.read_exact(&mut record_header).unwrap();
            assert_eq!(record_header[0], 22, "the first record must be TLS");
            let remaining = usize::from(u16::from_be_bytes([record_header[3], record_header[4]]));
            assert!(remaining <= 16 * 1024);
            let mut handshake = vec![0; remaining];
            connection.read_exact(&mut handshake).unwrap();
            assert!(!handshake
                .windows(b"synthetic-bearer".len())
                .any(|window| window == b"synthetic-bearer"));
            // Drop the socket instead of proving a certificate.
            observed_tx.send(()).unwrap();
        });
        let result = invoke(GatewayTurn {
            origin: &origin,
            tls: &GatewayTls::Loopback(
                TlsConfig::builder()
                    .root_certs(RootCerts::new_with_certs(&[]))
                    .build(),
            ),
            configured_model: "agenticos/example/model",
            model_id: "example/model",
            effort: "off",
            request_id: "synthetic-request",
            workspace_id: "ws_synthetic",
            max_output_tokens: 64,
            tool_choice: "none",
            messages: &[NativeMessageV1::User {
                content: vec![TextPartV1 {
                    text: "Synthetic prompt".into(),
                    signature: None,
                }],
                timestamp: 1,
            }],
            tools: &[],
            bearer: b"synthetic-bearer",
            deadline: Duration::from_secs(3),
        })
        .unwrap();
        assert!(matches!(result, ChildOutcome::Unknown { .. }));
        observed_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn gateway_accepts_prefer_json_schema_hint_and_refuses_required_or_grammar_constraints() {
        let small = [user_message("Reply plainly".into())];
        let read_params = json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string"}
            }
        });
        let prefer = NativeToolV1::parse(&json!({
            "name": "read",
            "description": "Read a file",
            "parameters": read_params,
            "constrainedSampling": {"type": "json_schema", "strict": "prefer"}
        }))
        .unwrap();
        let mut turn = fixture_turn(&GatewayTls::Production, &small);
        turn.tool_choice = "auto";
        turn.tools = std::slice::from_ref(&prefer);
        let wire: Value = serde_json::from_slice(&request(&turn).unwrap()).unwrap();
        assert_eq!(
            wire["tools"],
            json!([{
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "Read a file",
                    "parameters": read_params
                }
            }])
        );

        let required = NativeToolV1::parse(&json!({
            "name": "read",
            "description": "Read a file",
            "parameters": read_params,
            "constrainedSampling": {"type": "json_schema", "strict": "require"}
        }))
        .unwrap();
        turn.tools = std::slice::from_ref(&required);
        assert_eq!(
            request(&turn).unwrap_err().code(),
            Some("unsupported_content")
        );

        let grammar = NativeToolV1::parse(&json!({
            "name": "read",
            "description": "Read a file",
            "parameters": read_params,
            "constrainedSampling": {
                "type": "grammar",
                "variants": {"openai_lark": "start: \"ok\""}
            }
        }))
        .unwrap();
        turn.tools = std::slice::from_ref(&grammar);
        assert_eq!(
            request(&turn).unwrap_err().code(),
            Some("unsupported_content")
        );

        assert!(!admits_tool_constrained_sampling(Some(&json!({
            "type": "json_schema",
            "strict": "prefer",
            "extra": true
        }))));
    }
}
