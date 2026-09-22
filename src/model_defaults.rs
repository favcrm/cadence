//! Daemon-wide model defaults and the registration-time resolver.
//!
//! Defaults are a host policy for future registrations. They never
//! rewrite an existing agent, and they never change runtime `pm` /
//! `worker` authority. Team role is a separate lookup key.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::adapter::registry;
use crate::error::{Error, Result};

pub const MAX_MODEL_BYTES: usize = 200;
pub const MAX_CONFIG_BYTES: usize = 16 * 1024;
pub const MAX_HTTP_BODY_BYTES: usize = 20 * 1024;
pub const MAX_STORED_PARAMS: usize = 4_000;
/// Pre-serde duplicate-key scan limit. A body under 20 KiB can still
/// nest thousands of arrays; serde's recursion guard never runs if this
/// walk overflows the stack first.
const MAX_JSON_DEPTH: usize = 128;

/// Canonical team-role keys. `ops` is a launch-input alias of `devops`
/// and is not itself a stored key.
pub const TEAM_ROLES: &[(&str, &str)] = &[
    ("pm", "PM"),
    ("research", "Research"),
    ("architect", "Architect"),
    ("dev", "Developer"),
    ("qa", "QA"),
    ("devops", "DevOps"),
    ("worker", "Worker"),
];

pub const SOURCE_EXPLICIT: &str = "explicit";
pub const SOURCE_EXPLICIT_PROVIDER_DEFAULT: &str = "explicit_provider_default";
pub const SOURCE_ROLE_DEFAULT: &str = "role_default";
pub const SOURCE_PROVIDER_BASELINE: &str = "provider_baseline";
pub const SOURCE_PROVIDER_DEFAULT: &str = "provider_default";
pub const SOURCE_LEGACY_CONFIGURED: &str = "legacy_configured";
pub const SOURCE_LEGACY_PROVIDER_DEFAULT: &str = "legacy_provider_default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelPolicy {
    Inherit,
    ProviderDefault,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelSelector {
    Model { model: String },
    ProviderDefault,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDefaults {
    pub default: ModelSelector,
    #[serde(deserialize_with = "unique_string_map")]
    pub roles: BTreeMap<String, ModelSelector>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDefaults {
    pub schema: u32,
    #[serde(deserialize_with = "unique_string_map")]
    pub providers: BTreeMap<String, ProviderDefaults>,
}

impl ModelDefaults {
    pub fn empty() -> Self {
        Self {
            schema: 1,
            providers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsWrite {
    pub expected_revision: i64,
    pub config: ModelDefaults,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub actor: String,
    pub transport: &'static str,
}

pub struct ResolveRequest<'a> {
    pub provider: &'a str,
    pub endpoint_kind: &'a str,
    pub runtime_role: &'a str,
    pub team_role: Option<&'a str>,
    pub model_policy: Option<&'a str>,
    pub params: Option<&'a str>,
    pub config: &'a ModelDefaults,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRegistration {
    pub params: Option<String>,
    pub team_role: Option<String>,
    pub model_selection: Option<Value>,
}

/// One agent's observed model strings, used only to label suggestions.
pub struct ObservedModel<'a> {
    pub provider: &'a str,
    pub configured: Option<&'a str>,
    pub reported: Option<&'a str>,
}

fn unique_string_map<'de, D, T>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct MapVisitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for MapVisitor<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an object with unique string keys")
        }
        fn visit_map<A>(self, mut access: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut map = BTreeMap::new();
            while let Some(key) = access.next_key::<String>()? {
                if map.contains_key(&key) {
                    return Err(de::Error::custom(format!("duplicate key '{key}'")));
                }
                map.insert(key, access.next_value()?);
            }
            Ok(map)
        }
    }
    deserializer.deserialize_map(MapVisitor(std::marker::PhantomData))
}

/// Walk JSON and reject duplicate object keys before serde's last-key
/// map can hide them. The walk is also the size and integer-revision gate
/// for a settings body. Nesting deeper than [`MAX_JSON_DEPTH`] is rejected
/// so the scan cannot overflow the daemon stack.
struct JsonScan<'a> {
    bytes: &'a [u8],
    i: usize,
}

impl<'a> JsonScan<'a> {
    fn new(raw: &'a str) -> Self {
        Self {
            bytes: raw.as_bytes(),
            i: 0,
        }
    }

    fn fail(message: impl Into<String>) -> Error {
        Error::invalid("invalid_config", message)
    }

    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.i), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Result<u8> {
        self.bytes
            .get(self.i)
            .copied()
            .ok_or_else(|| Self::fail("unexpected end of JSON"))
    }

    fn bump(&mut self) -> Result<u8> {
        let byte = self.peek()?;
        self.i += 1;
        Ok(byte)
    }

    fn expect(&mut self, want: u8) -> Result<()> {
        let got = self.bump()?;
        if got == want {
            Ok(())
        } else {
            Err(Self::fail(format!(
                "expected '{}', found '{}'",
                want as char, got as char
            )))
        }
    }

    fn hex4(&mut self) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = self.bump()?;
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(Self::fail("invalid unicode escape")),
            };
            value = (value << 4) | u32::from(digit);
        }
        Ok(value)
    }

    fn parse_string(&mut self) -> Result<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let byte = self.bump()?;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let escaped = self.bump()?;
                    match escaped {
                        b'"' | b'\\' | b'/' => out.push(escaped as char),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let unit = self.hex4()?;
                            if (0xD800..=0xDBFF).contains(&unit) {
                                if self.bump()? != b'\\' || self.bump()? != b'u' {
                                    return Err(Self::fail("lone unicode surrogate"));
                                }
                                let low = self.hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return Err(Self::fail("invalid unicode surrogate pair"));
                                }
                                let point = 0x10000 + (((unit - 0xD800) << 10) | (low - 0xDC00));
                                out.push(
                                    char::from_u32(point)
                                        .ok_or_else(|| Self::fail("invalid unicode scalar"))?,
                                );
                            } else if (0xDC00..=0xDFFF).contains(&unit) {
                                return Err(Self::fail("lone unicode surrogate"));
                            } else {
                                out.push(
                                    char::from_u32(unit)
                                        .ok_or_else(|| Self::fail("invalid unicode scalar"))?,
                                );
                            }
                        }
                        _ => return Err(Self::fail("invalid string escape")),
                    }
                }
                0x00..=0x1F => return Err(Self::fail("unescaped control character in string")),
                byte if byte < 0x80 => out.push(byte as char),
                byte => {
                    let width = match byte {
                        0xC2..=0xDF => 2,
                        0xE0..=0xEF => 3,
                        0xF0..=0xF4 => 4,
                        _ => return Err(Self::fail("invalid UTF-8 in string")),
                    };
                    let start = self.i - 1;
                    let end = start + width;
                    if end > self.bytes.len() {
                        return Err(Self::fail("truncated UTF-8 in string"));
                    }
                    let text = std::str::from_utf8(&self.bytes[start..end])
                        .map_err(|_| Self::fail("invalid UTF-8 in string"))?;
                    out.push_str(text);
                    self.i = end;
                }
            }
        }
    }

    fn parse_number(&mut self) -> Result<()> {
        if self.peek()? == b'-' {
            self.i += 1;
        }
        match self.peek()? {
            b'0' => self.i += 1,
            b'1'..=b'9' => {
                while self.bytes.get(self.i).is_some_and(|b| b.is_ascii_digit()) {
                    self.i += 1;
                }
            }
            _ => return Err(Self::fail("invalid number")),
        }
        if self.bytes.get(self.i) == Some(&b'.') {
            self.i += 1;
            let start = self.i;
            while self.bytes.get(self.i).is_some_and(|b| b.is_ascii_digit()) {
                self.i += 1;
            }
            if self.i == start {
                return Err(Self::fail("invalid number"));
            }
        }
        if matches!(self.bytes.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.bytes.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let start = self.i;
            while self.bytes.get(self.i).is_some_and(|b| b.is_ascii_digit()) {
                self.i += 1;
            }
            if self.i == start {
                return Err(Self::fail("invalid number"));
            }
        }
        Ok(())
    }

    fn parse_literal(&mut self, literal: &[u8]) -> Result<()> {
        if self.bytes[self.i..].starts_with(literal) {
            self.i += literal.len();
            Ok(())
        } else {
            Err(Self::fail("invalid literal"))
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<()> {
        if depth >= MAX_JSON_DEPTH {
            return Err(Self::fail(format!("JSON nesting exceeds {MAX_JSON_DEPTH}")));
        }
        self.skip_ws();
        match self.peek()? {
            b'{' => self.parse_object(None, depth),
            b'[' => self.parse_array(depth),
            b'"' => {
                self.parse_string()?;
                Ok(())
            }
            b't' => self.parse_literal(b"true"),
            b'f' => self.parse_literal(b"false"),
            b'n' => self.parse_literal(b"null"),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => Err(Self::fail("invalid JSON value")),
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<()> {
        self.expect(b'[')?;
        self.skip_ws();
        if self.peek()? == b']' {
            self.i += 1;
            return Ok(());
        }
        loop {
            self.parse_value(depth + 1)?;
            self.skip_ws();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Ok(());
                }
                _ => return Err(Self::fail("expected ',' or ']'")),
            }
        }
    }

    fn parse_object(
        &mut self,
        mut capture: Option<&mut EnvelopeCapture>,
        depth: usize,
    ) -> Result<()> {
        if depth >= MAX_JSON_DEPTH {
            return Err(Self::fail(format!("JSON nesting exceeds {MAX_JSON_DEPTH}")));
        }
        self.expect(b'{')?;
        self.skip_ws();
        if self.peek()? == b'}' {
            self.i += 1;
            return Ok(());
        }
        let mut keys = HashSet::new();
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            if !keys.insert(key.clone()) {
                return Err(Self::fail(format!("duplicate key '{key}'")));
            }
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let start = self.i;
            if capture.is_some() {
                match key.as_str() {
                    "expected_revision" | "config" => {}
                    other => {
                        return Err(Error::invalid(
                            "invalid_request",
                            format!("unknown settings field '{other}'"),
                        ))
                    }
                }
            }
            self.parse_value(depth + 1)?;
            if let Some(captured) = capture.as_mut() {
                let lexeme = std::str::from_utf8(&self.bytes[start..self.i])
                    .map_err(|_| Self::fail("settings field was not UTF-8"))?
                    .to_string();
                match key.as_str() {
                    "expected_revision" => captured.revision_lexeme = Some(lexeme),
                    "config" => captured.config_len = Some(lexeme.len()),
                    _ => {}
                }
            }
            self.skip_ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Ok(());
                }
                _ => return Err(Self::fail("expected ',' or '}'")),
            }
        }
    }
}

struct EnvelopeCapture {
    revision_lexeme: Option<String>,
    config_len: Option<usize>,
}

fn revision_from_lexeme(lexeme: &str) -> Result<i64> {
    if lexeme.is_empty() || !lexeme.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::invalid(
            "invalid_request",
            "expected_revision must be a nonnegative integer",
        ));
    }
    if lexeme.len() > 1 && lexeme.starts_with('0') {
        return Err(Error::invalid(
            "invalid_request",
            "expected_revision must be a nonnegative integer",
        ));
    }
    let value: u64 = lexeme.parse().map_err(|_| {
        Error::invalid(
            "invalid_request",
            "expected_revision must be a nonnegative integer",
        )
    })?;
    i64::try_from(value).map_err(|_| {
        Error::invalid(
            "invalid_request",
            "expected_revision is outside the stored integer range",
        )
    })
}

/// Parse a settings save body. Duplicate keys are rejected from the
/// original text, and the config value itself must be at most 16 KiB.
pub fn parse_settings_document(raw: &str) -> Result<SettingsWrite> {
    if raw.len() > MAX_HTTP_BODY_BYTES {
        return Err(Error::invalid(
            "invalid_request",
            format!("settings body exceeds {MAX_HTTP_BODY_BYTES} bytes"),
        ));
    }
    let mut scan = JsonScan::new(raw);
    scan.skip_ws();
    if scan.peek()? != b'{' {
        return Err(Error::invalid(
            "invalid_request",
            "settings body must be a JSON object",
        ));
    }
    let mut captured = EnvelopeCapture {
        revision_lexeme: None,
        config_len: None,
    };
    scan.parse_object(Some(&mut captured), 0)?;
    scan.skip_ws();
    if scan.i != scan.bytes.len() {
        return Err(Error::invalid(
            "invalid_request",
            "trailing data after settings JSON",
        ));
    }
    let Some(lexeme) = captured.revision_lexeme else {
        return Err(Error::invalid(
            "invalid_request",
            "settings body requires expected_revision",
        ));
    };
    let Some(config_len) = captured.config_len else {
        return Err(Error::invalid(
            "invalid_request",
            "settings body requires config",
        ));
    };
    if config_len > MAX_CONFIG_BYTES {
        return Err(Error::invalid(
            "invalid_config",
            format!("model defaults document exceeds {MAX_CONFIG_BYTES} bytes"),
        ));
    }
    let expected_revision = revision_from_lexeme(&lexeme)?;
    let mut body: SettingsBody = serde_json::from_str(raw).map_err(|err| {
        Error::invalid(
            "invalid_config",
            format!("model defaults document was rejected: {err}"),
        )
    })?;
    validate_config(&mut body.config)?;
    Ok(SettingsWrite {
        expected_revision,
        config: body.config,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsBody {
    #[allow(dead_code)]
    expected_revision: u64,
    config: ModelDefaults,
}

pub fn validate_model_id(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::invalid(
            "invalid_model",
            "model id must be a non-empty string",
        ));
    }
    if trimmed.len() > MAX_MODEL_BYTES {
        return Err(Error::invalid(
            "invalid_model",
            format!("model id must be at most {MAX_MODEL_BYTES} bytes"),
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(Error::invalid(
            "invalid_model",
            "model id must not contain control characters",
        ));
    }
    Ok(trimmed.to_string())
}

pub fn canonical_team_role(role: &str) -> bool {
    TEAM_ROLES.iter().any(|(id, _)| *id == role)
}

/// Launch input only. `ops` becomes `devops`; unknown roles are rejected
/// rather than stored as a new authority.
pub fn normalize_team_role(raw: &str) -> Result<String> {
    let key = raw.trim().to_ascii_lowercase();
    if key.is_empty() {
        return Err(Error::invalid(
            "invalid_team_role",
            "team role must be a non-empty string",
        ));
    }
    let key = if key == "ops" { "devops" } else { key.as_str() };
    if canonical_team_role(key) {
        Ok(key.to_string())
    } else {
        Err(Error::invalid(
            "invalid_team_role",
            format!(
                "unknown team role '{raw}' — expected one of: {}",
                TEAM_ROLES
                    .iter()
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ))
    }
}

pub fn parse_model_policy(raw: Option<&str>) -> Result<ModelPolicy> {
    match raw.map(str::trim) {
        None => Ok(ModelPolicy::Inherit),
        Some("inherit") => Ok(ModelPolicy::Inherit),
        Some("provider_default") => Ok(ModelPolicy::ProviderDefault),
        Some(other) => Err(Error::invalid(
            "invalid_model_policy",
            format!("model_policy must be 'inherit' or 'provider_default', not '{other}'"),
        )),
    }
}

fn normalize_selector(selector: ModelSelector) -> Result<ModelSelector> {
    match selector {
        ModelSelector::ProviderDefault => Ok(ModelSelector::ProviderDefault),
        ModelSelector::Model { model } => Ok(ModelSelector::Model {
            model: validate_model_id(&model)?,
        }),
    }
}

pub fn validate_config(config: &mut ModelDefaults) -> Result<()> {
    if config.schema != 1 {
        return Err(Error::invalid(
            "invalid_config",
            "model defaults schema must be 1",
        ));
    }
    let eligible = registry::model_provider_ids();
    for (provider, entry) in &mut config.providers {
        if !eligible.iter().any(|id| *id == provider) {
            return Err(Error::invalid(
                "invalid_config",
                format!("provider '{provider}' does not accept a model default"),
            ));
        }
        entry.default = normalize_selector(entry.default.clone())?;
        for (role, selector) in &mut entry.roles {
            if !canonical_team_role(role) {
                return Err(Error::invalid(
                    "invalid_config",
                    format!("unknown team role '{role}' in model defaults"),
                ));
            }
            *selector = normalize_selector(selector.clone())?;
        }
    }
    Ok(())
}

pub fn normalize_attribution(raw: Option<&str>) -> Result<Attribution> {
    match raw {
        None => Ok(Attribution {
            actor: "local".to_string(),
            transport: "local",
        }),
        Some(value) => {
            let actor = value.trim();
            if actor.is_empty() || actor.len() > 160 || actor.chars().any(char::is_control) {
                return Err(Error::invalid(
                    "invalid_request",
                    "attribution must be 1-160 characters without control characters",
                ));
            }
            Ok(Attribution {
                actor: actor.to_string(),
                transport: "board",
            })
        }
    }
}

fn selection(
    source: &str,
    lookup_role: &str,
    revision: Option<i64>,
    model: Option<String>,
) -> Value {
    json!({
        "source": source,
        "lookup_role": lookup_role,
        "revision": revision,
        "model": model,
    })
}

/// Existing rows keep their params. Supported endpoints with no stored
/// provenance are labeled legacy so the board does not invent a source.
pub fn legacy_selection(
    supports: bool,
    team_role: Option<&str>,
    runtime_role: &str,
    configured: Option<&str>,
) -> Value {
    if !supports {
        return Value::Null;
    }
    let source = if configured.is_some() {
        SOURCE_LEGACY_CONFIGURED
    } else {
        SOURCE_LEGACY_PROVIDER_DEFAULT
    };
    selection(
        source,
        team_role.unwrap_or(runtime_role),
        None,
        configured.map(str::to_string),
    )
}

fn parse_params(raw: Option<&str>) -> Result<Map<String, Value>> {
    let Some(raw) = raw else {
        return Ok(Map::new());
    };
    let parsed: Value =
        serde_json::from_str(raw).map_err(|_| Error::rejected("params must be a JSON object"))?;
    match parsed {
        Value::Object(map) => Ok(map),
        _ => Err(Error::rejected("params must be a JSON object")),
    }
}

fn finalize_params(original: Option<&str>, merged: &Map<String, Value>) -> Result<Option<String>> {
    if merged.is_empty() {
        return Ok(None);
    }
    if let Some(original) = original {
        if let Ok(Value::Object(previous)) = serde_json::from_str::<Value>(original) {
            if &previous == merged {
                if original.len() > MAX_STORED_PARAMS {
                    return Err(Error::invalid(
                        "params_too_large",
                        "params must be a JSON object of at most 4000 characters",
                    ));
                }
                return Ok(Some(original.to_string()));
            }
        }
    }
    let text = Value::Object(merged.clone()).to_string();
    if text.len() > MAX_STORED_PARAMS {
        return Err(Error::invalid(
            "params_too_large",
            "params must be a JSON object of at most 4000 characters",
        ));
    }
    Ok(Some(text))
}

fn explicit_model(params: &Map<String, Value>) -> Result<Option<String>> {
    match params.get("model") {
        None => Ok(None),
        Some(Value::String(model)) => Ok(Some(validate_model_id(model)?)),
        Some(_) => Err(Error::invalid(
            "invalid_model",
            "model must be a non-empty string",
        )),
    }
}

fn apply_selector(
    params: &mut Map<String, Value>,
    selector: &ModelSelector,
    source: &str,
    lookup_role: &str,
    revision: i64,
) -> Value {
    match selector {
        ModelSelector::Model { model } => {
            params.insert("model".to_string(), json!(model));
            selection(source, lookup_role, Some(revision), Some(model.clone()))
        }
        ModelSelector::ProviderDefault => {
            params.remove("model");
            selection(source, lookup_role, Some(revision), None)
        }
    }
}

fn inherit(
    params: &mut Map<String, Value>,
    config: &ModelDefaults,
    provider: &str,
    lookup_role: &str,
    revision: i64,
) -> Value {
    let Some(entry) = config.providers.get(provider) else {
        params.remove("model");
        return selection(SOURCE_PROVIDER_DEFAULT, lookup_role, Some(revision), None);
    };
    if let Some(selector) = entry.roles.get(lookup_role) {
        return apply_selector(params, selector, SOURCE_ROLE_DEFAULT, lookup_role, revision);
    }
    let source = match entry.default {
        ModelSelector::Model { .. } => SOURCE_PROVIDER_BASELINE,
        ModelSelector::ProviderDefault => SOURCE_PROVIDER_DEFAULT,
    };
    apply_selector(params, &entry.default, source, lookup_role, revision)
}

/// Resolve one fresh registration against a defaults snapshot already
/// read inside the caller's write transaction.
pub fn resolve(request: ResolveRequest<'_>) -> Result<ResolvedRegistration> {
    let team_role = match request.team_role {
        Some(raw) => Some(normalize_team_role(raw)?),
        None => None,
    };
    let policy = parse_model_policy(request.model_policy)?;
    let mut params = parse_params(request.params)?;
    let explicit = explicit_model(&params)?;
    if explicit.is_some() && policy == ModelPolicy::ProviderDefault {
        return Err(Error::invalid(
            "conflicting_model_policy",
            "an explicit model cannot be combined with model_policy provider_default",
        ));
    }
    if !registry::supports_model(request.provider, request.endpoint_kind) {
        if policy == ModelPolicy::ProviderDefault {
            return Err(Error::invalid(
                "unsupported_model_setting",
                format!(
                    "provider '{}' endpoint '{}' does not accept a model",
                    request.provider, request.endpoint_kind
                ),
            ));
        }
        return Ok(ResolvedRegistration {
            params: finalize_params(request.params, &params)?,
            team_role,
            model_selection: None,
        });
    }
    let lookup = team_role
        .clone()
        .unwrap_or_else(|| request.runtime_role.to_string());
    let model_selection = if let Some(model) = explicit {
        params.insert("model".to_string(), json!(model.clone()));
        selection(SOURCE_EXPLICIT, &lookup, None, Some(model))
    } else if policy == ModelPolicy::ProviderDefault {
        params.remove("model");
        selection(SOURCE_EXPLICIT_PROVIDER_DEFAULT, &lookup, None, None)
    } else {
        inherit(
            &mut params,
            request.config,
            request.provider,
            &lookup,
            request.revision,
        )
    };
    Ok(ResolvedRegistration {
        params: finalize_params(request.params, &params)?,
        team_role,
        model_selection: Some(model_selection),
    })
}

/// Provenance for an explicit next-launch model change. Clearing the
/// model records provider-native behavior for that agent only.
pub fn explicit_override_selection(lookup_role: &str, model: Option<&str>) -> Result<Value> {
    match model {
        None => Ok(selection(
            SOURCE_EXPLICIT_PROVIDER_DEFAULT,
            lookup_role,
            None,
            None,
        )),
        Some(model) => {
            let model = validate_model_id(model)?;
            Ok(selection(SOURCE_EXPLICIT, lookup_role, None, Some(model)))
        }
    }
}

pub fn snapshot_json(revision: i64, config: &ModelDefaults, observed: &[ObservedModel]) -> Value {
    let providers: Vec<Value> = registry::model_provider_matrix()
        .iter()
        .map(|row| {
            let mut suggestions = BTreeSet::new();
            if row.eligible {
                for item in observed.iter().filter(|item| item.provider == row.id) {
                    for candidate in [item.configured, item.reported].into_iter().flatten() {
                        if let Ok(model) = validate_model_id(candidate) {
                            suggestions.insert(model);
                        }
                    }
                }
            }
            json!({
                "id": row.id,
                "label": provider_label(row.id),
                "eligible": row.eligible,
                "kinds": row.kinds,
                "suggestions": suggestions.into_iter().collect::<Vec<_>>(),
                "suggestions_note": "Previously observed on agents of this provider. Not a provider catalog.",
                "limitation": row.limitation,
            })
        })
        .collect();
    json!({
        "revision": revision,
        "config": config,
        "providers": providers,
        "roles": TEAM_ROLES.iter().map(|(id, label)| json!({"id": id, "label": label})).collect::<Vec<_>>(),
    })
}

fn provider_label(id: &str) -> &str {
    match id {
        "codex" => "Codex",
        "claude" => "Claude",
        "cursor" => "Cursor",
        "devin" => "Devin",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(raw: &str) -> ModelDefaults {
        let mut parsed: ModelDefaults = serde_json::from_str(raw).unwrap();
        validate_config(&mut parsed).unwrap();
        parsed
    }

    fn resolved(
        provider: &str,
        kind: &str,
        role: &str,
        team: Option<&str>,
        policy: Option<&str>,
        params: Option<&str>,
        defaults: &ModelDefaults,
    ) -> ResolvedRegistration {
        resolve(ResolveRequest {
            provider,
            endpoint_kind: kind,
            runtime_role: role,
            team_role: team,
            model_policy: policy,
            params,
            config: defaults,
            revision: 4,
        })
        .unwrap()
    }

    fn source(resolved: &ResolvedRegistration) -> String {
        resolved.model_selection.as_ref().unwrap()["source"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn precedence_follows_explicit_role_baseline_then_native() {
        let defaults = config(
            r#"{"schema":1,"providers":{
                "claude":{
                    "default":{"mode":"model","model":"baseline"},
                    "roles":{
                        "qa":{"mode":"model","model":"qa-model"},
                        "dev":{"mode":"provider_default"},
                        "worker":{"mode":"model","model":"worker-model"}
                    }
                },
                "codex":{"default":{"mode":"provider_default"},"roles":{}}
            }}"#,
        );
        let explicit = resolved(
            "claude",
            "managed",
            "worker",
            Some("qa"),
            None,
            Some(r#"{"model":" explicit "}"#),
            &defaults,
        );
        assert_eq!(source(&explicit), "explicit");
        assert_eq!(
            explicit.model_selection.as_ref().unwrap()["revision"],
            json!(null)
        );
        assert_eq!(explicit.params.as_deref(), Some(r#"{"model":"explicit"}"#));

        let role = resolved("claude", "pty", "worker", Some("qa"), None, None, &defaults);
        assert_eq!(source(&role), "role_default");
        assert_eq!(role.model_selection.as_ref().unwrap()["model"], "qa-model");
        assert_eq!(role.model_selection.as_ref().unwrap()["revision"], 4);

        let native_role = resolved(
            "claude",
            "managed",
            "worker",
            Some("dev"),
            None,
            None,
            &defaults,
        );
        assert_eq!(source(&native_role), "role_default");
        assert_eq!(
            native_role.model_selection.as_ref().unwrap()["model"],
            json!(null)
        );
        assert!(native_role.params.is_none());

        let baseline = resolved(
            "claude",
            "managed",
            "worker",
            Some("architect"),
            None,
            None,
            &defaults,
        );
        assert_eq!(source(&baseline), "provider_baseline");
        assert_eq!(
            baseline.model_selection.as_ref().unwrap()["model"],
            "baseline"
        );

        let worker = resolved("claude", "managed", "worker", None, None, None, &defaults);
        assert_eq!(
            worker.model_selection.as_ref().unwrap()["lookup_role"],
            "worker"
        );
        assert_eq!(
            worker.model_selection.as_ref().unwrap()["model"],
            "worker-model"
        );

        let qa_ignores_worker = resolved(
            "claude",
            "managed",
            "worker",
            Some("research"),
            None,
            None,
            &defaults,
        );
        assert_eq!(
            qa_ignores_worker.model_selection.as_ref().unwrap()["model"],
            "baseline"
        );

        let ops = resolved(
            "claude",
            "managed",
            "worker",
            Some(" OPS "),
            None,
            None,
            &defaults,
        );
        assert_eq!(ops.team_role.as_deref(), Some("devops"));
        assert_eq!(
            ops.model_selection.as_ref().unwrap()["lookup_role"],
            "devops"
        );

        let bypass = resolved(
            "claude",
            "managed",
            "worker",
            Some("qa"),
            Some("provider_default"),
            None,
            &defaults,
        );
        assert_eq!(source(&bypass), "explicit_provider_default");
        assert_eq!(
            bypass.model_selection.as_ref().unwrap()["revision"],
            json!(null)
        );
        assert!(bypass.params.is_none());

        let codex = resolved(
            "codex",
            "managed-ws",
            "worker",
            Some("qa"),
            None,
            None,
            &defaults,
        );
        assert_eq!(source(&codex), "provider_default");
        assert!(codex.params.is_none());

        let empty = ModelDefaults::empty();
        let omitted = resolved("cursor", "pty", "pm", None, None, None, &empty);
        assert_eq!(source(&omitted), "provider_default");
        assert_eq!(omitted.model_selection.as_ref().unwrap()["revision"], 4);
        assert_eq!(
            omitted.model_selection.as_ref().unwrap()["lookup_role"],
            "pm"
        );
    }

    #[test]
    fn unsupported_endpoints_do_not_receive_a_model() {
        let defaults = config(
            r#"{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"baseline"},"roles":{}}}}"#,
        );
        let devin = resolved(
            "devin",
            "pty",
            "worker",
            Some("qa"),
            None,
            Some(r#"{"permission_mode":"auto"}"#),
            &defaults,
        );
        assert!(devin.model_selection.is_none());
        assert_eq!(devin.team_role.as_deref(), Some("qa"));
        assert_eq!(
            devin.params.as_deref(),
            Some(r#"{"permission_mode":"auto"}"#)
        );
        let err = resolve(ResolveRequest {
            provider: "inbox",
            endpoint_kind: "inbox",
            runtime_role: "worker",
            team_role: None,
            model_policy: Some("provider_default"),
            params: None,
            config: &defaults,
            revision: 1,
        })
        .unwrap_err();
        assert_eq!(err.code(), Some("unsupported_model_setting"));
    }

    #[test]
    fn invalid_documents_and_conflicts_reject() {
        let err = parse_settings_document(r#"{"expected_revision":0,"config":{"schema":1,"providers":{"nope":{"default":{"mode":"provider_default"},"roles":{}}}}}"#).unwrap_err();
        assert_eq!(err.code(), Some("invalid_config"));
        let err = parse_settings_document(
            r#"{"expected_revision":0,"expected_revision":1,"config":{"schema":1,"providers":{}}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate key"));
        let err = parse_settings_document(
            r#"{"expected_revision":0,"config":{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"a"},"roles":{"qa":{"mode":"model","model":"b"},"qa":{"mode":"provider_default"}}}}}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate key"));
        let err = parse_settings_document(
            r#"{"expected_revision":0,"config":{"schema":1,"providers":{},"extra":true}}"#,
        )
        .unwrap_err();
        assert_eq!(err.code(), Some("invalid_config"));
        let err = parse_settings_document(
            r#"{"expected_revision":1.5,"config":{"schema":1,"providers":{}}}"#,
        )
        .unwrap_err();
        assert_eq!(err.code(), Some("invalid_request"));
        let err = parse_settings_document(
            r#"{"expected_revision":-1,"config":{"schema":1,"providers":{}}}"#,
        )
        .unwrap_err();
        assert_eq!(err.code(), Some("invalid_request"));
        let mut bad = config(
            r#"{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"ok"},"roles":{}}}}"#,
        );
        bad.providers.get_mut("claude").unwrap().default = ModelSelector::Model {
            model: " \n ".to_string(),
        };
        assert_eq!(
            validate_config(&mut bad).unwrap_err().code(),
            Some("invalid_model")
        );
        let long = "m".repeat(201);
        assert!(validate_model_id(&long).is_err());
        assert!(validate_model_id("bad\u{0007}id").is_err());
        let defaults = ModelDefaults::empty();
        let err = resolve(ResolveRequest {
            provider: "claude",
            endpoint_kind: "managed",
            runtime_role: "worker",
            team_role: None,
            model_policy: Some("provider_default"),
            params: Some(r#"{"model":"explicit"}"#),
            config: &defaults,
            revision: 0,
        })
        .unwrap_err();
        assert_eq!(err.code(), Some("conflicting_model_policy"));
        let err = resolve(ResolveRequest {
            provider: "claude",
            endpoint_kind: "managed",
            runtime_role: "worker",
            team_role: None,
            model_policy: None,
            params: Some(r#"{"model":"   "}"#),
            config: &defaults,
            revision: 0,
        })
        .unwrap_err();
        assert_eq!(err.code(), Some("invalid_model"));
        let err = resolve(ResolveRequest {
            provider: "claude",
            endpoint_kind: "managed",
            runtime_role: "worker",
            team_role: None,
            model_policy: None,
            params: Some(r#"{"model":1}"#),
            config: &defaults,
            revision: 0,
        })
        .unwrap_err();
        assert_eq!(err.code(), Some("invalid_model"));
        assert!(normalize_team_role("nope").is_err());
        let huge = format!(
            r#"{{"expected_revision":0,"config":{{"schema":1,"providers":{{"claude":{{"default":{{"mode":"model","model":"{}"}},"roles":{{}}}}}}}}}}"#,
            "x".repeat(MAX_CONFIG_BYTES)
        );
        assert_eq!(
            parse_settings_document(&huge).unwrap_err().code(),
            Some("invalid_config")
        );
    }

    #[test]
    fn deep_json_nesting_is_rejected() {
        let nest = 4_000;
        let mut body = String::from(r#"{"expected_revision":0,"config":"#);
        body.push_str(&"[".repeat(nest));
        body.push('0');
        body.push_str(&"]".repeat(nest));
        body.push('}');
        assert!(
            body.len() <= MAX_HTTP_BODY_BYTES,
            "fixture must stay under the body cap, got {}",
            body.len()
        );
        let err = parse_settings_document(&body).unwrap_err();
        assert_eq!(err.code(), Some("invalid_config"));
        assert!(err.to_string().contains("nesting exceeds"), "{err}");
    }

    #[test]
    fn identical_selector_round_trips_and_trims() {
        let write = parse_settings_document(
            r#"{"expected_revision":0,"config":{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"  opus  "},"roles":{"qa":{"mode":"provider_default"}}}}}}"#,
        )
        .unwrap();
        assert_eq!(write.expected_revision, 0);
        match &write.config.providers["claude"].default {
            ModelSelector::Model { model } => assert_eq!(model, "opus"),
            other => panic!("{other:?}"),
        }
    }
}
