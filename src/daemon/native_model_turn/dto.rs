//! Strict typed DTOs for the bounded native model operation.
//!
//! The guest nominates *content*, never authority. Every frame here is parsed
//! against the exact r7 shape: exact key sets, canonical lowercase UUIDs,
//! canonical decimal counters/revisions (bounded by `i64`), safe integers,
//! bounded strings, the real Pi 1.4 message union, and no structural
//! ambiguity. Numbers are never coerced and duplicates never survive.
//!
//! The projection to the private SDK child is the same Pi union the guest
//! sent; the broker only narrows what the union admits (no raw diagnostics,
//! no deferred handles, no provider wire data).

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::protected_runtime::strict;
use cadence_core::connection_dispatch::canonical_value;

pub(crate) const MAX_INVOCATION_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_RECEIPT_BYTES: usize = MAX_INVOCATION_BYTES + 8 * 1024;
pub(crate) const MAX_MESSAGES: usize = 100;
pub(crate) const MAX_ACTIVE_TOOLS: usize = 16;
pub(crate) const MAX_TOOL_CALLS: usize = 16;
pub(crate) const MAX_TEXT_BYTES: usize = 64_000;
pub(crate) const MAX_DESCRIPTION_BYTES: usize = 4_000;
pub(crate) const MAX_TOOL_NAME: usize = 64;
pub(crate) const MAX_TOOL_ID: usize = 200;
pub(crate) const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_OUTPUT_TOKENS_CAP: u32 = 8_192;
pub(crate) const MAX_DEPTH: usize = 16;

fn refused(code: &'static str, message: &str) -> Error {
    Error::invalid(code, message)
}

fn invalid() -> Error {
    refused("invalid_request", "the model request is malformed")
}

fn unsupported() -> Error {
    refused("unsupported_content", "the model content is not supported")
}

fn canonical_uuid(value: &str) -> Result<String> {
    let b = value.as_bytes();
    if b.len() != 36 {
        return Err(invalid());
    }
    for (i, &c) in b.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if c != b'-' {
                return Err(invalid());
            }
        } else if !(c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
            return Err(invalid());
        }
    }
    Ok(value.to_string())
}

/// A canonical decimal string (`0|[1-9][0-9]*`, <= i64::MAX). JSON numbers
/// refuse: the wire carries counters/revisions as strings.
fn decimal(value: &Value, positive: bool) -> Result<u64> {
    let text = value.as_str().ok_or_else(invalid)?;
    if text.is_empty() || text.len() > 19 {
        return Err(invalid());
    }
    if text.as_bytes()[0] == b'0' && text.len() != 1 {
        return Err(invalid());
    }
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let parsed: u64 = text.parse().map_err(|_| invalid())?;
    if parsed > i64::MAX as u64 || (positive && parsed == 0) {
        return Err(invalid());
    }
    Ok(parsed)
}

fn decimal_field(m: &Map<String, Value>, key: &str, positive: bool) -> Result<u64> {
    let value = m.get(key).ok_or_else(invalid)?;
    decimal(value, positive)
}

/// Safe non-negative integer as a JSON number (timestamps, token counts).
fn safe(value: &Value) -> Result<u64> {
    match value.as_u64() {
        Some(n) if n <= 9_007_199_254_740_991 => Ok(n),
        _ => Err(invalid()),
    }
}

fn safe_field(m: &Map<String, Value>, key: &str) -> Result<u64> {
    let value = m.get(key).ok_or_else(invalid)?;
    safe(value)
}

fn required_str<'a>(m: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    m.get(key).and_then(Value::as_str).ok_or_else(invalid)
}

fn string<'a>(m: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    let value = required_str(m, key)?;
    if value.len() > MAX_TEXT_BYTES || !is_valid_utf8_text(value) {
        return Err(unsupported());
    }
    Ok(value)
}

fn is_valid_utf8_text(value: &str) -> bool {
    // `serde_json` already guarantees UTF-8; refuse NUL and lone-surrogate
    // escapes never survive parsing, but a NUL byte is not valid transcript
    // content for this transport.
    !value.as_bytes().contains(&0)
}

fn bounded_name(value: &str) -> Result<&str> {
    if value.is_empty()
        || value.len() > MAX_TOOL_NAME
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(unsupported());
    }
    Ok(value)
}

fn bounded_tool_id(value: &str) -> Result<&str> {
    if value.is_empty() || value.len() > MAX_TOOL_ID || value.as_bytes().contains(&0) {
        return Err(unsupported());
    }
    Ok(value)
}

/// Reject non-safe-integer numbers anywhere in a bounded document: the
/// canonical digest and the provider protocol both admit integers only.
pub(crate) fn validate_integer_json(value: &Value, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(refused(
            "payload_too_large",
            "the model request is too deep",
        ));
    }
    match value {
        Value::Number(number) => {
            if number.as_i64().is_some() || number.as_u64().is_some() {
                let within = number
                    .as_u64()
                    .map(|n| n <= 9_007_199_254_740_991)
                    .unwrap_or_else(|| {
                        number
                            .as_i64()
                            .map(|n| n >= -(9_007_199_254_740_991_i64))
                            .unwrap_or(false)
                            && number
                                .as_i64()
                                .map(|n| n <= 9_007_199_254_740_991_i64)
                                .unwrap_or(false)
                    });
                if !within {
                    return Err(refused(
                        "payload_too_large",
                        "the model request carries an out-of-range number",
                    ));
                }
                Ok(())
            } else {
                Err(unsupported())
            }
        }
        Value::Array(items) => {
            for item in items {
                validate_integer_json(item, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for item in fields.values() {
                validate_integer_json(item, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// `sha256("cadence.tool.schema.v1\0" + canonical(parameters))` — the exact
/// digest the pinned observer's registry measures, not a second convention.
pub(crate) fn tool_schema_digest(parameters: &Value) -> Result<String> {
    validate_integer_json(parameters, 0)?;
    let canonical =
        serde_json::to_string(&canonical_value(parameters)).map_err(|_| unsupported())?;
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"cadence.tool.schema.v1\0");
    hasher.update(canonical.as_bytes());
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// The credential-free canonical digest of typed input/options + source
/// revision. Secret-bearing bytes are never an input.
pub(crate) fn material_digest(domain: &str, value: &Value) -> Result<String> {
    validate_integer_json(value, 0)?;
    Ok(cadence_core::connection_dispatch::material_digest_for_domain(domain, value))
}

// ---------------------------------------------------------------------------
// Content union
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextPartV1 {
    pub text: String,
    pub signature: Option<String>,
}

impl TextPartV1 {
    fn parse(value: &Value) -> Result<Self> {
        let m = strict::object_with(value, &["type", "text"], &["textSignature"])?;
        if strict::string(m, "type")? != "text" {
            return Err(unsupported());
        }
        let text = string(m, "text")?.to_string();
        let signature = match m.get("textSignature") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_str()
                    .filter(|s| s.len() <= MAX_TEXT_BYTES)
                    .ok_or_else(unsupported)?
                    .to_string(),
            ),
        };
        Ok(Self { text, signature })
    }

    fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("type".into(), json!("text"));
        m.insert("text".into(), json!(self.text));
        if let Some(signature) = &self.signature {
            m.insert("textSignature".into(), json!(signature));
        }
        Value::Object(m)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ThinkingPartV1 {
    pub thinking: String,
    pub signature: Option<String>,
    pub redacted: bool,
}

impl ThinkingPartV1 {
    fn parse(value: &Value) -> Result<Self> {
        let m = strict::object_with(
            value,
            &["type", "thinking"],
            &["thinkingSignature", "redacted"],
        )?;
        if strict::string(m, "type")? != "thinking" {
            return Err(unsupported());
        }
        let redacted = match m.get("redacted") {
            None => false,
            Some(value) => value.as_bool().ok_or_else(invalid)?,
        };
        Ok(Self {
            thinking: string(m, "thinking")?.to_string(),
            signature: match m.get("thinkingSignature") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .filter(|s| s.len() <= MAX_TEXT_BYTES)
                        .ok_or_else(unsupported)?
                        .to_string(),
                ),
            },
            redacted,
        })
    }

    fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("type".into(), json!("thinking"));
        m.insert("thinking".into(), json!(self.thinking));
        if let Some(signature) = &self.signature {
            m.insert("thinkingSignature".into(), json!(signature));
        }
        if self.redacted {
            m.insert("redacted".into(), json!(true));
        }
        Value::Object(m)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToolCallPartV1 {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    pub thought_signature: Option<String>,
    pub namespace: Option<String>,
    pub arguments_error: Option<String>,
}

impl ToolCallPartV1 {
    fn parse(value: &Value) -> Result<Self> {
        let m = strict::object_with(
            value,
            &["type", "id", "name", "arguments"],
            &["thoughtSignature", "namespace", "argumentsError"],
        )?;
        if strict::string(m, "type")? != "toolCall" {
            return Err(unsupported());
        }
        let arguments = m
            .get("arguments")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(unsupported)?;
        validate_integer_json(&arguments, 0)?;
        let optional = |key: &str| -> Result<Option<String>> {
            match m.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => value
                    .as_str()
                    .filter(|s| s.len() <= MAX_TEXT_BYTES)
                    .map(|s| Some(s.to_string()))
                    .ok_or_else(unsupported),
            }
        };
        Ok(Self {
            id: bounded_tool_id(required_str(m, "id")?)?.to_string(),
            name: bounded_name(required_str(m, "name")?)?.to_string(),
            arguments,
            thought_signature: optional("thoughtSignature")?,
            namespace: optional("namespace")?,
            arguments_error: optional("argumentsError")?,
        })
    }

    fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("type".into(), json!("toolCall"));
        m.insert("id".into(), json!(self.id));
        m.insert("name".into(), json!(self.name));
        m.insert("arguments".into(), self.arguments.clone());
        if let Some(value) = &self.thought_signature {
            m.insert("thoughtSignature".into(), json!(value));
        }
        if let Some(value) = &self.namespace {
            m.insert("namespace".into(), json!(value));
        }
        if let Some(value) = &self.arguments_error {
            m.insert("argumentsError".into(), json!(value));
        }
        Value::Object(m)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AssistantPartV1 {
    Text(TextPartV1),
    Thinking(ThinkingPartV1),
    ToolCall(ToolCallPartV1),
}

impl AssistantPartV1 {
    fn parse(value: &Value) -> Result<Self> {
        let kind = value
            .as_object()
            .and_then(|m| m.get("type"))
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        match kind {
            "text" => Ok(Self::Text(TextPartV1::parse(value)?)),
            "thinking" => Ok(Self::Thinking(ThinkingPartV1::parse(value)?)),
            "toolCall" => Ok(Self::ToolCall(ToolCallPartV1::parse(value)?)),
            _ => Err(unsupported()),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Text(part) => part.to_json(),
            Self::Thinking(part) => part.to_json(),
            Self::ToolCall(part) => part.to_json(),
        }
    }
}

/// Pi's `Usage` as carried by historical assistant turns. Cost is
/// informational only and is never converted into a local bill.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PiUsageV1 {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
}

impl PiUsageV1 {
    fn parse(value: &Value) -> Result<Self> {
        let m = strict::object_with(
            value,
            &[
                "input",
                "output",
                "cacheRead",
                "cacheWrite",
                "totalTokens",
                "cost",
            ],
            &["cacheWrite1h", "reasoning"],
        )?;
        let cost = m.get("cost").ok_or_else(invalid)?;
        let cost = strict::object(
            cost,
            &["input", "output", "cacheRead", "cacheWrite", "total"],
        )?;
        for key in ["input", "output", "cacheRead", "cacheWrite", "total"] {
            safe_field(cost, key)?;
        }
        Ok(Self {
            input: safe_field(m, "input")?,
            output: safe_field(m, "output")?,
            cache_read: safe_field(m, "cacheRead")?,
            cache_write: safe_field(m, "cacheWrite")?,
            total: safe_field(m, "totalTokens")?,
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "input": self.input,
            "output": self.output,
            "cacheRead": self.cache_read,
            "cacheWrite": self.cache_write,
            "totalTokens": self.total,
            "cost": {"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0},
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NativeToolV1 {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub constrained_sampling: Option<Value>,
}

impl NativeToolV1 {
    pub(crate) fn parse(value: &Value) -> Result<Self> {
        let m = strict::object_with(
            value,
            &["name", "description", "parameters"],
            &["constrainedSampling"],
        )?;
        let name = bounded_name(required_str(m, "name")?)?.to_string();
        let description = required_str(m, "description")?;
        if description.len() > MAX_DESCRIPTION_BYTES {
            return Err(refused(
                "payload_too_large",
                "the tool description is too large",
            ));
        }
        let parameters = m.get("parameters").cloned().ok_or_else(invalid)?;
        if !parameters.is_object() {
            return Err(unsupported());
        }
        validate_integer_json(&parameters, 0)?;
        let constrained_sampling = match m.get("constrainedSampling") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => None,
            Some(value) => Some(validate_constrained_sampling(value)?),
        };
        Ok(Self {
            name,
            description: description.to_string(),
            parameters,
            constrained_sampling,
        })
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("name".into(), json!(self.name));
        m.insert("description".into(), json!(self.description));
        m.insert("parameters".into(), self.parameters.clone());
        if let Some(value) = &self.constrained_sampling {
            m.insert("constrainedSampling".into(), value.clone());
        }
        Value::Object(m)
    }

    pub(crate) fn schema_digest(&self) -> Result<String> {
        tool_schema_digest(&self.parameters)
    }

    /// The credential-free digest of the whole declaration (name,
    /// description, parameters, constrained sampling). The frozen
    /// configuration binds this for every approved tool.
    pub(crate) fn declaration_digest(&self) -> Result<String> {
        material_digest("cadence-native-model-tool-declaration-v1", &self.to_json())
    }
}

/// The installed SDK's exact serializable constrained-sampling union.
fn validate_constrained_sampling(value: &Value) -> Result<Value> {
    let m = value.as_object().ok_or_else(unsupported)?;
    match strict::string(m, "type")? {
        "json_schema" => {
            strict::object(value, &["type", "strict"])?;
            let strict_mode = strict::string(m, "strict")?;
            if !matches!(strict_mode, "prefer" | "require") {
                return Err(unsupported());
            }
            Ok(value.clone())
        }
        "grammar" => {
            strict::object(value, &["type", "variants"])?;
            let variants = m
                .get("variants")
                .and_then(Value::as_object)
                .ok_or_else(unsupported)?;
            for (key, entry) in variants {
                if !matches!(key.as_str(), "openai_lark" | "openai_regex") {
                    return Err(unsupported());
                }
                let text = entry.as_str().ok_or_else(unsupported)?;
                if text.len() > MAX_TEXT_BYTES {
                    return Err(unsupported());
                }
            }
            Ok(value.clone())
        }
        _ => Err(unsupported()),
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AssistantHistoryV1 {
    pub content: Vec<AssistantPartV1>,
    pub api: String,
    pub provider: String,
    pub model: String,
    pub response_model: Option<String>,
    pub response_id: Option<String>,
    pub provider_thinking_level: Option<String>,
    pub thinking_level: Option<String>,
    pub usage: PiUsageV1,
    pub stop_reason: String,
    pub timestamp: u64,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NativeMessageV1 {
    System {
        content: Vec<TextPartV1>,
        sections: Option<Map<String, Value>>,
        tools_added: Vec<NativeToolV1>,
        tools_removed: Vec<String>,
        timestamp: u64,
    },
    User {
        content: Vec<TextPartV1>,
        timestamp: u64,
    },
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        content: Vec<TextPartV1>,
        is_error: bool,
        timestamp: u64,
        duration_ms: Option<u64>,
    },
    Assistant(AssistantHistoryV1),
}

fn text_parts(value: &Value) -> Result<Vec<TextPartV1>> {
    let items = value.as_array().ok_or_else(unsupported)?;
    let mut parts = Vec::with_capacity(items.len());
    for item in items {
        parts.push(TextPartV1::parse(item)?);
    }
    Ok(parts)
}

/// `string | Text[]` content, normalized to `Text[]` for the projection.
fn text_or_array(m: &Map<String, Value>, key: &str) -> Result<Vec<TextPartV1>> {
    match m.get(key) {
        Some(Value::String(text)) => {
            if text.len() > MAX_TEXT_BYTES || text.as_bytes().contains(&0) {
                return Err(unsupported());
            }
            Ok(vec![TextPartV1 {
                text: text.clone(),
                signature: None,
            }])
        }
        Some(Value::Array(_)) => text_parts(m.get(key).expect("checked")),
        _ => Err(unsupported()),
    }
}

impl NativeMessageV1 {
    pub(crate) fn parse(value: &Value) -> Result<Self> {
        let role = value
            .as_object()
            .and_then(|m| m.get("role"))
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        match role {
            "system" => {
                let m = strict::object_with(
                    value,
                    &["role", "content", "timestamp"],
                    &["sections", "toolsAdded", "toolsRemoved"],
                )?;
                let content = text_or_array(m, "content")?;
                let sections = match m.get("sections") {
                    None | Some(Value::Null) => None,
                    Some(value) => {
                        let sections = value.as_object().ok_or_else(invalid)?;
                        if sections.len() > 32 {
                            return Err(refused(
                                "payload_too_large",
                                "the model request is too large",
                            ));
                        }
                        for (name, body) in sections {
                            if name.is_empty() || name.len() > 128 {
                                return Err(unsupported());
                            }
                            if !body.is_null() {
                                let text = body.as_str().ok_or_else(unsupported)?;
                                if text.len() > MAX_TEXT_BYTES {
                                    return Err(refused(
                                        "payload_too_large",
                                        "the model request is too large",
                                    ));
                                }
                            }
                        }
                        Some(sections.clone())
                    }
                };
                let tools_added = match m.get("toolsAdded") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(value) => {
                        let items = value.as_array().ok_or_else(unsupported)?;
                        if items.len() > MAX_ACTIVE_TOOLS {
                            return Err(refused("payload_too_large", "too many tool declarations"));
                        }
                        items
                            .iter()
                            .map(NativeToolV1::parse)
                            .collect::<Result<Vec<_>>>()?
                    }
                };
                let tools_removed = match m.get("toolsRemoved") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(value) => {
                        let items = value.as_array().ok_or_else(unsupported)?;
                        if items.len() > MAX_ACTIVE_TOOLS {
                            return Err(refused("payload_too_large", "too many tool removals"));
                        }
                        items
                            .iter()
                            .map(|item| {
                                let m = strict::object(item, &["name"])?;
                                Ok(bounded_name(required_str(m, "name")?)?.to_string())
                            })
                            .collect::<Result<Vec<_>>>()?
                    }
                };
                Ok(Self::System {
                    content,
                    sections,
                    tools_added,
                    tools_removed,
                    timestamp: safe_field(m, "timestamp")?,
                })
            }
            "user" => {
                let m = strict::object(value, &["role", "content", "timestamp"])?;
                let content = match m.get("content") {
                    Some(Value::String(text)) => {
                        if text.len() > MAX_TEXT_BYTES || text.as_bytes().contains(&0) {
                            return Err(unsupported());
                        }
                        vec![TextPartV1 {
                            text: text.clone(),
                            signature: None,
                        }]
                    }
                    Some(Value::Array(items)) => {
                        let mut parts = Vec::with_capacity(items.len());
                        for item in items {
                            let content = TextPartV1::parse(item)?;
                            parts.push(content);
                        }
                        parts
                    }
                    _ => return Err(unsupported()),
                };
                Ok(Self::User {
                    content,
                    timestamp: safe_field(m, "timestamp")?,
                })
            }
            "toolResult" => {
                let m = strict::object_with(
                    value,
                    &[
                        "role",
                        "toolCallId",
                        "toolName",
                        "content",
                        "isError",
                        "timestamp",
                    ],
                    &["durationMs"],
                )?;
                let content = text_parts(m.get("content").ok_or_else(invalid)?)?;
                let is_error = strict::boolean(m, "isError")?;
                Ok(Self::ToolResult {
                    tool_call_id: bounded_tool_id(required_str(m, "toolCallId")?)?.to_string(),
                    tool_name: bounded_name(required_str(m, "toolName")?)?.to_string(),
                    content,
                    is_error,
                    timestamp: safe_field(m, "timestamp")?,
                    duration_ms: optional_safe(m, "durationMs")?,
                })
            }
            "assistant" => {
                let m = strict::object_with(
                    value,
                    &[
                        "role",
                        "content",
                        "api",
                        "provider",
                        "model",
                        "usage",
                        "stopReason",
                        "timestamp",
                    ],
                    &[
                        "responseModel",
                        "responseId",
                        "providerThinkingLevel",
                        "thinkingLevel",
                        "durationMs",
                    ],
                )?;
                let items = m
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or_else(unsupported)?;
                let mut content = Vec::with_capacity(items.len());
                let mut tool_calls = 0usize;
                for item in items {
                    let part = AssistantPartV1::parse(item)?;
                    if matches!(part, AssistantPartV1::ToolCall(_)) {
                        tool_calls += 1;
                    }
                    content.push(part);
                }
                if tool_calls > MAX_TOOL_CALLS {
                    return Err(refused(
                        "payload_too_large",
                        "too many tool calls in one assistant message",
                    ));
                }
                let stop_reason = strict::string(m, "stopReason")?.to_string();
                if !matches!(stop_reason.as_str(), "stop" | "length" | "toolUse") {
                    return Err(unsupported());
                }
                let api = strict::string(m, "api")?.to_string();
                let provider = strict::string(m, "provider")?.to_string();
                let model = strict::string(m, "model")?.to_string();
                if api.len() > 128 || provider.len() > 128 || model.len() > 200 {
                    return Err(unsupported());
                }
                Ok(Self::Assistant(AssistantHistoryV1 {
                    content,
                    api,
                    provider,
                    model,
                    response_model: optional_string(m, "responseModel")?,
                    response_id: optional_string(m, "responseId")?,
                    provider_thinking_level: optional_string(m, "providerThinkingLevel")?,
                    thinking_level: optional_string(m, "thinkingLevel")?,
                    usage: PiUsageV1::parse(m.get("usage").ok_or_else(invalid)?)?,
                    stop_reason,
                    timestamp: safe_field(m, "timestamp")?,
                    duration_ms: optional_safe(m, "durationMs")?,
                }))
            }
            _ => Err(unsupported()),
        }
    }

    /// The Pi 1.4 wire projection forwarded to the private SDK child.
    pub(crate) fn to_pi_json(&self) -> Value {
        match self {
            Self::System {
                content,
                sections,
                tools_added,
                tools_removed,
                timestamp,
            } => {
                let mut m = Map::new();
                m.insert("role".into(), json!("system"));
                m.insert(
                    "content".into(),
                    Value::Array(content.iter().map(TextPartV1::to_json).collect()),
                );
                if let Some(sections) = sections {
                    m.insert("sections".into(), Value::Object(sections.clone()));
                }
                if !tools_added.is_empty() {
                    m.insert(
                        "toolsAdded".into(),
                        Value::Array(tools_added.iter().map(NativeToolV1::to_json).collect()),
                    );
                }
                if !tools_removed.is_empty() {
                    m.insert(
                        "toolsRemoved".into(),
                        Value::Array(
                            tools_removed
                                .iter()
                                .map(|name| json!({"name": name}))
                                .collect(),
                        ),
                    );
                }
                m.insert("timestamp".into(), json!(timestamp));
                Value::Object(m)
            }
            Self::User { content, timestamp } => json!({
                "role": "user",
                "content": content.iter().map(TextPartV1::to_json).collect::<Vec<_>>(),
                "timestamp": timestamp,
            }),
            Self::ToolResult {
                tool_call_id,
                tool_name,
                content,
                is_error,
                timestamp,
                duration_ms,
            } => {
                let mut m = Map::new();
                m.insert("role".into(), json!("toolResult"));
                m.insert("toolCallId".into(), json!(tool_call_id));
                m.insert("toolName".into(), json!(tool_name));
                m.insert(
                    "content".into(),
                    Value::Array(content.iter().map(TextPartV1::to_json).collect()),
                );
                m.insert("isError".into(), json!(is_error));
                m.insert("timestamp".into(), json!(timestamp));
                if let Some(duration) = duration_ms {
                    m.insert("durationMs".into(), json!(duration));
                }
                Value::Object(m)
            }
            Self::Assistant(assistant) => {
                let mut m = Map::new();
                m.insert("role".into(), json!("assistant"));
                m.insert(
                    "content".into(),
                    Value::Array(assistant.content.iter().map(|p| p.to_json()).collect()),
                );
                m.insert("api".into(), json!(assistant.api));
                m.insert("provider".into(), json!(assistant.provider));
                m.insert("model".into(), json!(assistant.model));
                if let Some(value) = &assistant.response_model {
                    m.insert("responseModel".into(), json!(value));
                }
                if let Some(value) = &assistant.response_id {
                    m.insert("responseId".into(), json!(value));
                }
                if let Some(value) = &assistant.provider_thinking_level {
                    m.insert("providerThinkingLevel".into(), json!(value));
                }
                if let Some(value) = &assistant.thinking_level {
                    m.insert("thinkingLevel".into(), json!(value));
                }
                m.insert("usage".into(), assistant.usage.to_json());
                m.insert("stopReason".into(), json!(assistant.stop_reason));
                m.insert("timestamp".into(), json!(assistant.timestamp));
                if let Some(duration) = assistant.duration_ms {
                    m.insert("durationMs".into(), json!(duration));
                }
                Value::Object(m)
            }
        }
    }

    pub(crate) fn declared_tools(&self) -> &[NativeToolV1] {
        match self {
            Self::System { tools_added, .. } => tools_added,
            _ => &[],
        }
    }
}

fn optional_string(m: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|s| s.len() <= MAX_TEXT_BYTES && !s.as_bytes().contains(&0))
            .map(|s| Some(s.to_string()))
            .ok_or_else(unsupported),
    }
}

fn optional_safe(m: &Map<String, Value>, key: &str) -> Result<Option<u64>> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(safe(value)?)),
    }
}

// ---------------------------------------------------------------------------
// Frame arguments
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PrepareArgsV1 {
    pub request_index: u64,
}

pub(crate) fn parse_prepare_args(v: &Value) -> Result<PrepareArgsV1> {
    let m = strict::object(v, &["v", "request_index"])?;
    if strict::safe_u64(m, "v")? != 1 {
        return Err(invalid());
    }
    Ok(PrepareArgsV1 {
        request_index: decimal_field(m, "request_index", false)?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolChoiceV1 {
    Auto,
    None,
}

impl ToolChoiceV1 {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InvokeArgsV1 {
    pub request_id: String,
    pub configuration_id: String,
    pub expected_configuration_revision: u64,
    pub snapshot_id: String,
    pub loaded_receipt_id: String,
    pub messages: Vec<NativeMessageV1>,
    pub max_output_tokens: u32,
    pub tool_choice: ToolChoiceV1,
}

pub(crate) fn parse_invoke_args(v: &Value) -> Result<InvokeArgsV1> {
    let m = strict::object(
        v,
        &[
            "v",
            "request_id",
            "configuration_id",
            "expected_configuration_revision",
            "snapshot_id",
            "loaded_receipt_id",
            "messages",
            "max_output_tokens",
            "tool_choice",
        ],
    )?;
    if strict::safe_u64(m, "v")? != 1 {
        return Err(invalid());
    }
    let items = m
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    if items.is_empty() || items.len() > MAX_MESSAGES {
        return Err(refused(
            "payload_too_large",
            "the model request message count is out of bounds",
        ));
    }
    let mut messages = Vec::with_capacity(items.len());
    for item in items {
        messages.push(NativeMessageV1::parse(item)?);
    }
    let max_output_tokens = safe_field(m, "max_output_tokens")?;
    if max_output_tokens == 0 || max_output_tokens > MAX_OUTPUT_TOKENS_CAP as u64 {
        return Err(refused(
            "payload_too_large",
            "the model output bound is out of range",
        ));
    }
    let tool_choice = match strict::string(m, "tool_choice")? {
        "auto" => ToolChoiceV1::Auto,
        "none" => ToolChoiceV1::None,
        _ => return Err(invalid()),
    };
    let args = InvokeArgsV1 {
        request_id: canonical_uuid(required_str(m, "request_id")?)?,
        configuration_id: canonical_uuid(required_str(m, "configuration_id")?)?,
        expected_configuration_revision: decimal_field(m, "expected_configuration_revision", true)?,
        snapshot_id: canonical_uuid(required_str(m, "snapshot_id")?)?,
        loaded_receipt_id: canonical_uuid(required_str(m, "loaded_receipt_id")?)?,
        messages,
        max_output_tokens: max_output_tokens as u32,
        tool_choice,
    };
    let total = serde_json::to_vec(v).map_err(|_| invalid())?;
    if total.len() > MAX_INVOCATION_BYTES {
        return Err(refused(
            "payload_too_large",
            "the model invocation exceeds its bound",
        ));
    }
    Ok(args)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResultArgsV1 {
    pub request_id: String,
}

pub(crate) fn parse_result_args(v: &Value) -> Result<ResultArgsV1> {
    let m = strict::object(v, &["v", "request_id"])?;
    if strict::safe_u64(m, "v")? != 1 {
        return Err(invalid());
    }
    Ok(ResultArgsV1 {
        request_id: canonical_uuid(required_str(m, "request_id")?)?,
    })
}

// ---------------------------------------------------------------------------
// The active tool set and the canonical operation digest
// ---------------------------------------------------------------------------

/// Resolve the active declarations after replaying every system delta. This
/// mirrors Pi's `getCurrentTools`: a later complete declaration replaces the
/// earlier one; a removal drops it.
pub(crate) fn active_tools(messages: &[NativeMessageV1]) -> Result<Vec<NativeToolV1>> {
    let mut current: Vec<NativeToolV1> = Vec::new();
    for message in messages {
        if let NativeMessageV1::System {
            tools_added,
            tools_removed,
            ..
        } = message
        {
            for tool in tools_added {
                if let Some(existing) = current.iter_mut().find(|t| t.name == tool.name) {
                    *existing = tool.clone();
                } else {
                    if current.len() >= MAX_ACTIVE_TOOLS {
                        return Err(refused(
                            "payload_too_large",
                            "too many active tool declarations",
                        ));
                    }
                    current.push(tool.clone());
                }
            }
            for name in tools_removed {
                current.retain(|tool| &tool.name != name);
            }
        }
    }
    Ok(current)
}

/// The credential-free typed input/options binding. The same value is the
/// durable input digest and the child commitment; no secret or provider body
/// can enter it.
pub(crate) fn operation_binding(args: &InvokeArgsV1, active: &[NativeToolV1]) -> Result<Value> {
    let mut messages = Vec::with_capacity(args.messages.len());
    for message in &args.messages {
        messages.push(message.to_pi_json());
    }
    let mut tools = Vec::with_capacity(active.len());
    for tool in active {
        tools.push(tool.to_json());
    }
    let value = json!({
        "v": 1,
        "messages": messages,
        "tools": tools,
        "tool_choice": args.tool_choice.as_str(),
        "max_output_tokens": args.max_output_tokens,
    });
    validate_integer_json(&value, 0)?;
    Ok(value)
}

// ---------------------------------------------------------------------------
// Assistant result, usage, closed errors and billing
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NativeAssistantV1 {
    pub content: Vec<AssistantPartV1>,
    pub api: String,
    pub provider: String,
    pub model: String,
    pub response_model: Option<String>,
    pub response_id: Option<String>,
    pub provider_thinking_level: Option<String>,
    pub thinking_level: Option<String>,
    pub usage: PiUsageV1,
    pub stop_reason: String,
    pub timestamp: u64,
    pub duration_ms: Option<u64>,
}

impl NativeAssistantV1 {
    /// Validate the private child's terminal assistant projection. This is
    /// the only shape a guest can reconstruct a stream from.
    pub(crate) fn parse(value: &Value) -> Result<Self> {
        let m = strict::object_with(
            value,
            &[
                "role",
                "content",
                "api",
                "provider",
                "model",
                "usage",
                "stopReason",
                "timestamp",
            ],
            &[
                "responseModel",
                "responseId",
                "providerThinkingLevel",
                "thinkingLevel",
                "durationMs",
            ],
        )?;
        if strict::string(m, "role")? != "assistant" {
            return Err(refused(
                "result_refused",
                "the model result is not an assistant message",
            ));
        }
        let items = m
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| refused("result_refused", "the model result content is malformed"))?;
        let mut content = Vec::with_capacity(items.len());
        let mut tool_calls = 0usize;
        for item in items {
            let part = AssistantPartV1::parse(item)
                .map_err(|_| refused("result_refused", "the model result content is malformed"))?;
            if matches!(part, AssistantPartV1::ToolCall(_)) {
                tool_calls += 1;
            }
            content.push(part);
        }
        if tool_calls > MAX_TOOL_CALLS {
            return Err(refused(
                "result_refused",
                "the model result carries too many tool calls",
            ));
        }
        let stop_reason = strict::string(m, "stopReason")?.to_string();
        if !matches!(stop_reason.as_str(), "stop" | "length" | "toolUse") {
            return Err(refused(
                "result_refused",
                "the model result stop reason is not terminal",
            ));
        }
        let api = strict::string(m, "api")?.to_string();
        let provider = strict::string(m, "provider")?.to_string();
        let model = strict::string(m, "model")?.to_string();
        let usage = PiUsageV1::parse(m.get("usage").ok_or_else(|| {
            refused(
                "result_refused",
                "the model result has no usage observation",
            )
        })?)
        .map_err(|_| refused("result_refused", "the model result usage is malformed"))?;
        let assistant = Self {
            content,
            api,
            provider,
            model,
            response_model: optional_string(m, "responseModel")
                .map_err(|_| refused("result_refused", "the model result is malformed"))?,
            response_id: optional_string(m, "responseId")
                .map_err(|_| refused("result_refused", "the model result is malformed"))?,
            provider_thinking_level: optional_string(m, "providerThinkingLevel")
                .map_err(|_| refused("result_refused", "the model result is malformed"))?,
            thinking_level: optional_string(m, "thinkingLevel")
                .map_err(|_| refused("result_refused", "the model result is malformed"))?,
            usage,
            stop_reason,
            timestamp: safe_field(m, "timestamp")
                .map_err(|_| refused("result_refused", "the model result is malformed"))?,
            duration_ms: optional_safe(m, "durationMs")
                .map_err(|_| refused("result_refused", "the model result is malformed"))?,
        };
        let bytes = serde_json::to_vec(&assistant.to_json()).map_err(|_| invalid())?;
        if bytes.len() > MAX_OUTPUT_BYTES {
            return Err(refused(
                "payload_too_large",
                "the model result exceeds its bound",
            ));
        }
        Ok(assistant)
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("role".into(), json!("assistant"));
        m.insert(
            "content".into(),
            Value::Array(self.content.iter().map(|p| p.to_json()).collect()),
        );
        m.insert("api".into(), json!(self.api));
        m.insert("provider".into(), json!(self.provider));
        m.insert("model".into(), json!(self.model));
        if let Some(value) = &self.response_model {
            m.insert("responseModel".into(), json!(value));
        }
        if let Some(value) = &self.response_id {
            m.insert("responseId".into(), json!(value));
        }
        if let Some(value) = &self.provider_thinking_level {
            m.insert("providerThinkingLevel".into(), json!(value));
        }
        if let Some(value) = &self.thinking_level {
            m.insert("thinkingLevel".into(), json!(value));
        }
        m.insert("usage".into(), self.usage.to_json());
        m.insert("stopReason".into(), json!(self.stop_reason));
        m.insert("timestamp".into(), json!(self.timestamp));
        if let Some(duration) = self.duration_ms {
            m.insert("durationMs".into(), json!(duration));
        }
        Value::Object(m)
    }

    pub(crate) fn tool_calls(&self) -> impl Iterator<Item = &ToolCallPartV1> {
        self.content.iter().filter_map(|part| match part {
            AssistantPartV1::ToolCall(call) => Some(call),
            _ => None,
        })
    }
}

/// The actual provider usage observation. `present` distinguishes a real
/// observation from the SDK's fabricated zero default.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NativeUsageObservationV1 {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
}

impl NativeUsageObservationV1 {
    pub(crate) fn from_assistant(assistant: &NativeAssistantV1) -> Option<Self> {
        let usage = &assistant.usage;
        let sum = usage
            .input
            .checked_add(usage.output)?
            .checked_add(usage.cache_read)?
            .checked_add(usage.cache_write)?;
        if sum != usage.total {
            return None;
        }
        Some(Self {
            input: usage.input,
            output: usage.output,
            cache_read: usage.cache_read,
            cache_write: usage.cache_write,
            total: usage.total,
        })
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "v": 1,
            "present": true,
            "input": self.input,
            "output": self.output,
            "cache_read": self.cache_read,
            "cache_write": self.cache_write,
            "total": self.total,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ClosedModelTurnErrorV1 {
    pub code: &'static str,
    pub message: &'static str,
}

impl ClosedModelTurnErrorV1 {
    pub(crate) fn to_json(self) -> Value {
        json!({"v": 1, "code": self.code, "message": self.message})
    }

    pub(crate) fn invalid_request() -> Self {
        Self {
            code: "invalid_request",
            message: "the model request is malformed",
        }
    }
    pub(crate) fn payload_too_large() -> Self {
        Self {
            code: "payload_too_large",
            message: "the model request exceeds its bound",
        }
    }
    pub(crate) fn route_unavailable() -> Self {
        Self {
            code: "route_unavailable",
            message: "the model route is not available",
        }
    }
    pub(crate) fn auth_unavailable() -> Self {
        Self {
            code: "auth_unavailable",
            message: "the model authentication is not available",
        }
    }
    pub(crate) fn unsupported_transport() -> Self {
        Self {
            code: "unsupported_transport",
            message: "the model transport is not supported",
        }
    }
    pub(crate) fn unsupported_content() -> Self {
        Self {
            code: "unsupported_content",
            message: "the model content is not supported",
        }
    }
    pub(crate) fn result_refused() -> Self {
        Self {
            code: "result_refused",
            message: "the model result is not usable",
        }
    }
    pub(crate) fn outcome_unknown() -> Self {
        Self {
            code: "outcome_unknown",
            message: "the model outcome is unknown and remains reserved",
        }
    }
}

/// The upstream-owned billing projection. No local bill and no USD
/// conversion of Pi's floating cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeBillingProjectionV1 {
    pub route_kind: &'static str,
    pub status: &'static str,
}

impl NativeBillingProjectionV1 {
    pub(crate) const NOT_DISPATCHED: Self = Self {
        route_kind: "subscription",
        status: "not_dispatched",
    };
    pub(crate) const UPSTREAM_OWNED: Self = Self {
        route_kind: "subscription",
        status: "upstream_owned",
    };
    pub(crate) const UNKNOWN: Self = Self {
        route_kind: "subscription",
        status: "unknown",
    };

    pub(crate) fn to_json(self) -> Value {
        json!({"route_kind": self.route_kind, "status": self.status})
    }
}
