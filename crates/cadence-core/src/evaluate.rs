//! The one versioned JSON entry point every non-native host calls.
//!
//! Input `{v: 1, op, input}`; output `{ok: true, op, value}` (`op` echoes the request) or
//! `{ok: false, code, message}`. The rules live in typed Rust functions
//! ([`manifest_preflight`]); [`evaluate`] only decodes, dispatches and encodes.
//! Domain input only: not caller authentication, and not a host
//! compatibility decision (the native host stays authoritative for that).
use crate::app_compat::parse_requires;
use crate::frontmatter::split_front;
use crate::issue::{self, ReadinessInput, ReadinessValue, ValidateInput, ValidateValue};
use crate::works::{self, Decision, Draft, State, Violation};
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
    /// Validate a Works draft's kind, title, tags and closed content payload.
    #[serde(rename = "works.draft.validate")]
    WorksDraftValidate(DraftInput),
    /// Whether a Works review may be recorded for a revision.
    #[serde(rename = "works.review.check")]
    WorksReviewCheck(ReviewInput),
    /// Whether a Works revision is approved material a host may export.
    #[serde(rename = "works.export.check")]
    WorksExportCheck(ExportInput),
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

/// Input of `works.draft.validate`: the fields a Works create parses. They
/// stay untyped so a wrong type reaches the same refusal a native caller gets
/// instead of failing envelope decoding.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftInput {
    #[schemars(with = "Option<Value>")]
    pub kind: Option<Value>,
    #[schemars(with = "Option<Value>")]
    pub title: Option<Value>,
    #[schemars(with = "Option<Value>")]
    pub content: Option<Value>,
    /// Optional; an absent or null value means no tags.
    #[schemars(with = "Option<Value>")]
    pub tags: Option<Value>,
}

impl DraftInput {
    /// The create parameters the shared grammar parses, with absent fields
    /// still absent (an absent and a null `content` refuse differently).
    fn params(&self) -> Value {
        let mut params = serde_json::Map::new();
        for (key, value) in [
            ("kind", &self.kind),
            ("title", &self.title),
            ("content", &self.content),
            ("tags", &self.tags),
        ] {
            if let Some(value) = value {
                params.insert(key.to_string(), value.clone());
            }
        }
        Value::Object(params)
    }
}

/// One refused Works domain step, in the native wire shape.
#[derive(Serialize, JsonSchema)]
pub struct WorksRefusal {
    /// `rejected` or `conflict`.
    pub kind: String,
    /// `invalid` | `conflict` | `forbidden` | `not_found` | `needs_review`.
    pub code: String,
    pub message: String,
}

impl From<Violation> for WorksRefusal {
    fn from(violation: Violation) -> Self {
        Self {
            kind: violation.kind.into(),
            code: violation.code.into(),
            message: violation.message,
        }
    }
}

/// Success value of `works.draft.validate`. The two material fields are the
/// exact bytes a host hashes: a revision's `contentDigest` is `sha256:` plus
/// the hex SHA-256 of the UTF-8 `content_material` bytes, and
/// `payload_material` also covers tags.
#[derive(Serialize, JsonSchema)]
pub struct DraftValue {
    pub valid: bool,
    /// The normalized kind, present only when the draft is valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The trimmed title, present only when the draft is valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Tags after trimming and de-duplication, present only when valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_material: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_material: Option<String>,
    pub refusals: Vec<WorksRefusal>,
}

/// The stored state a Works operation must judge; the host proves it.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewInput {
    /// The item's current revision.
    pub current: i64,
    /// The revision under review.
    pub revision: i64,
    /// `none` | `pending` | `approved` | `changes_requested`.
    pub review_status: String,
    /// The revision a pending review was requested for.
    pub pending: Option<i64>,
    /// The stored author of the revision.
    pub author: String,
    /// The verified human presenting the decision.
    pub reviewer: String,
    /// `approve` or `changes`.
    pub decision: String,
    pub note: Option<String>,
}

/// Success value of `works.review.check`: the state the host must write.
#[derive(Serialize, JsonSchema)]
pub struct ReviewValue {
    pub allowed: bool,
    /// `approved` or `changes_requested`; absent when refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_status: Option<String>,
    /// The revision an approval records; absent when refused, or for
    /// `changes`, which leaves the stored reviewed revision unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed_revision: Option<i64>,
    pub refusals: Vec<WorksRefusal>,
}

/// Input of `works.export.check`: the stored state plus the requested format.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportInput {
    pub current: i64,
    pub revision: i64,
    pub review_status: String,
    /// The revision the last approval covered; `0` before any approval.
    pub reviewed: i64,
    pub trashed: bool,
    /// The host's export format, required, non-blank and bounded.
    pub format: String,
}

/// Success value of `works.export.check`.
#[derive(Serialize, JsonSchema)]
pub struct ExportValue {
    pub allowed: bool,
    /// `draft` | `edited` | `reviewed`.
    pub review_state: String,
    /// `ready` | `blocked`.
    pub export_state: String,
    /// The card's export explanation for this state.
    pub reason: String,
    pub refusals: Vec<WorksRefusal>,
}

pub fn draft_validate(input: &DraftInput) -> DraftValue {
    match Draft::parse(&input.params()) {
        Ok(draft) => DraftValue {
            valid: true,
            content_material: Some(draft.content_material()),
            payload_material: Some(draft.payload_material()),
            kind: Some(draft.kind),
            title: Some(draft.title),
            tags: Some(draft.tags),
            refusals: Vec::new(),
        },
        Err(violation) => DraftValue {
            valid: false,
            kind: None,
            title: None,
            tags: None,
            content_material: None,
            payload_material: None,
            refusals: vec![violation.into()],
        },
    }
}

pub fn review_check(input: &ReviewInput) -> ReviewValue {
    // The review gate reads current, review_status and pending; `reviewed`
    // and trash state are not part of it.
    let state = State {
        current: input.current,
        reviewed: 0,
        review_status: &input.review_status,
        pending: input.pending,
        trashed: false,
    };
    let admitted = works::review_decision(&input.decision, input.note.as_deref())
        .and_then(|decision| works::review(&state, input.revision).map(|()| decision))
        .and_then(|decision| works::reviewer(&input.author, &input.reviewer).map(|()| decision));
    match admitted {
        Ok(Decision::Approve) => ReviewValue {
            allowed: true,
            review_status: Some(Decision::Approve.status().into()),
            reviewed_revision: Some(input.revision),
            refusals: Vec::new(),
        },
        Ok(Decision::Changes) => ReviewValue {
            allowed: true,
            review_status: Some(Decision::Changes.status().into()),
            reviewed_revision: None,
            refusals: Vec::new(),
        },
        Err(violation) => ReviewValue {
            allowed: false,
            review_status: None,
            reviewed_revision: None,
            refusals: vec![violation.into()],
        },
    }
}

pub fn export_check(input: &ExportInput) -> ExportValue {
    let state = State {
        current: input.current,
        reviewed: input.reviewed,
        review_status: &input.review_status,
        pending: None,
        trashed: input.trashed,
    };
    let admitted =
        works::check_format(&input.format).and_then(|()| works::export(&state, input.revision));
    ExportValue {
        allowed: admitted.is_ok(),
        review_state: works::review_state(&state).into(),
        export_state: if works::export_state(&state) {
            "ready".into()
        } else {
            "blocked".into()
        },
        reason: works::export_reason(&state).into(),
        refusals: admitted.err().map(|v| vec![v.into()]).unwrap_or_default(),
    }
}

/// The success value of whichever operation ran, keyed by the echoed `op`.
#[derive(Serialize, JsonSchema)]
#[serde(tag = "op", content = "value")]
pub enum OpValue {
    #[serde(rename = "app.manifest.preflight")]
    Preflight(ManifestPreflightValue),
    #[serde(rename = "issue.validate")]
    Validate(ValidateValue),
    #[serde(rename = "issue.readiness")]
    Readiness(ReadinessValue),
    #[serde(rename = "works.draft.validate")]
    Draft(DraftValue),
    #[serde(rename = "works.review.check")]
    Review(ReviewValue),
    #[serde(rename = "works.export.check")]
    Export(ExportValue),
}

/// Success response.
#[derive(Serialize, JsonSchema)]
pub struct Success {
    #[schemars(extend("const" = true))]
    pub ok: bool,
    #[serde(flatten)]
    pub result: OpValue,
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
        Operation::WorksDraftValidate(input) => Ok(OpValue::Draft(draft_validate(&input))),
        Operation::WorksReviewCheck(input) => Ok(OpValue::Review(review_check(&input))),
        Operation::WorksExportCheck(input) => Ok(OpValue::Export(export_check(&input))),
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
        Ok(value) => encode(&Success {
            ok: true,
            result: value,
        }),
        Err(Refusal { code, message }) => refusal_bytes(code, &message),
    }
}
