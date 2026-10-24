//! The one versioned JSON entry point every non-native host calls.
//!
//! Input `{v: 1, op, input}`; output `{ok: true, value}` or
//! `{ok: false, code, message}`. The rules live in typed Rust functions
//! ([`manifest_preflight`]); [`evaluate`] only decodes, dispatches and encodes.
//! Domain input only: not caller authentication, and not a host
//! compatibility decision (the native host stays authoritative for that).
use crate::app_compat::parse_requires;
use crate::frontmatter::split_front;
use crate::issue::{self, ReadinessInput, ReadinessValue, ValidateInput, ValidateValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_yaml::Value as Yaml;

pub const CONTRACT_VERSION: u32 = 1;
/// Native `MAX_FILE_BYTES` (and the published bundle grammar's `fileBytes`).
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// A manifest can escape to six bytes per input byte inside the envelope.
pub const MAX_INPUT_BYTES: usize = 6 * MAX_MANIFEST_BYTES + 4_096;

/// Closed set of refusal codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    /// The envelope is malformed, oversize, unversioned or names no known op.
    InvalidCommand,
    /// The manifest is not a well-formed `app.md` with a valid `needs.requires`.
    InvalidManifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: Code,
    pub message: String,
}

fn refuse(code: Code, message: impl std::fmt::Display) -> Refusal {
    Refusal {
        code,
        message: message.to_string(),
    }
}

/// The request envelope.
#[derive(Deserialize, JsonSchema)]
pub struct Request {
    /// Contract version; only `1` exists.
    pub v: u32,
    #[serde(flatten)]
    pub operation: Operation,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "op", content = "input")]
pub enum Operation {
    /// Split the frontmatter, then check `needs.requires`.
    #[serde(rename = "app.manifest.preflight")]
    AppManifestPreflight(ManifestPreflightInput),
    /// Check an issue id, project key, tags, status and priority.
    #[serde(rename = "issue.validate")]
    IssueValidate(ValidateInput),
    /// Whether an issue is ready or blocked, given its blockers' statuses.
    #[serde(rename = "issue.readiness")]
    IssueReadiness(ReadinessInput),
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestPreflightInput {
    /// The exact `app.md` text.
    pub manifest: String,
}

/// Success value of `app.manifest.preflight`.
#[derive(Serialize, JsonSchema)]
pub struct ManifestPreflightValue {}

/// The success value of whichever operation ran.
#[derive(Serialize, JsonSchema)]
#[serde(untagged)]
pub enum OpValue {
    Preflight(ManifestPreflightValue),
    Validate(ValidateValue),
    Readiness(ReadinessValue),
}

/// Success response.
#[derive(Serialize, JsonSchema)]
pub struct Success {
    #[schemars(extend("const" = true))]
    pub ok: bool,
    pub value: OpValue,
}

/// Refusal response.
#[derive(Serialize, JsonSchema)]
pub struct Refused {
    #[schemars(extend("const" = false))]
    pub ok: bool,
    pub code: Code,
    /// Human-readable refusal reason.
    pub message: String,
}

/// Structural preflight only: admit a well-formed `needs.requires` block and
/// refuse a malformed one. Host compatibility is never inferred here.
pub fn manifest_preflight(manifest: &str) -> Result<(), Refusal> {
    if manifest.len() > MAX_MANIFEST_BYTES {
        return Err(refuse(
            Code::InvalidCommand,
            "manifest exceeds its byte limit",
        ));
    }
    let (yaml, _) = split_front(manifest).map_err(|error| refuse(Code::InvalidManifest, error))?;
    // `serde_yaml::Value` is also the native representation: duplicate
    // mapping keys refuse, and scalar typing survives.
    let Yaml::Mapping(frontmatter) =
        serde_yaml::from_str::<Yaml>(yaml).map_err(|error| refuse(Code::InvalidManifest, error))?
    else {
        return Err(refuse(
            Code::InvalidManifest,
            "frontmatter must be a mapping",
        ));
    };
    let needs = match frontmatter.get("needs") {
        // No declaration is a legacy package, exactly as natively parsed.
        None => None,
        // An explicit non-mapping `needs` (including null) is malformed.
        Some(Yaml::Mapping(needs)) => Some(needs),
        Some(_) => return Err(refuse(Code::InvalidManifest, "needs must be a mapping")),
    };
    parse_requires(needs).map_err(|error| refuse(Code::InvalidManifest, error))?;
    Ok(())
}

fn dispatch(input: &[u8]) -> Result<OpValue, Refusal> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(refuse(
            Code::InvalidCommand,
            "command exceeds its byte limit",
        ));
    }
    // Duplicate envelope fields refuse here; `serde_json::Value` would
    // silently collapse them.
    serde_json::from_slice::<Yaml>(input)
        .map_err(|_| refuse(Code::InvalidCommand, "command must be unambiguous JSON"))?;
    let value: Value = serde_json::from_slice(input)
        .map_err(|_| refuse(Code::InvalidCommand, "command must be valid JSON"))?;
    let Some(object) = value.as_object() else {
        return Err(refuse(Code::InvalidCommand, "command must be an object"));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "v" | "op" | "input"))
    {
        return Err(refuse(Code::InvalidCommand, "unknown command field"));
    }
    if object.get("v").and_then(Value::as_u64) != Some(CONTRACT_VERSION.into()) {
        return Err(refuse(Code::InvalidCommand, "unsupported contract version"));
    }
    let request: Request =
        serde_json::from_value(value).map_err(|error| refuse(Code::InvalidCommand, error))?;
    match request.operation {
        Operation::AppManifestPreflight(input) => {
            manifest_preflight(&input.manifest)?;
            Ok(OpValue::Preflight(ManifestPreflightValue {}))
        }
        Operation::IssueValidate(input) => Ok(OpValue::Validate(issue::validate(&input))),
        Operation::IssueReadiness(input) => Ok(OpValue::Readiness(issue::readiness(&input))),
    }
}

/// Encode a refusal exactly as [`evaluate`] does.
pub fn refusal_bytes(code: Code, message: &str) -> Vec<u8> {
    encode(&Refused {
        ok: false,
        code,
        message: message.into(),
    })
}

fn encode(value: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(value).expect("bounded JSON result")
}

pub fn evaluate(input: &[u8]) -> Vec<u8> {
    match dispatch(input) {
        Ok(value) => encode(&Success { ok: true, value }),
        Err(Refusal { code, message }) => refusal_bytes(code, &message),
    }
}
