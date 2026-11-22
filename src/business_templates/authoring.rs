//! Authoring pack parsing and reference asset validation for business templates.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

use super::{
    check_approvable, check_docx_package, html, parse_spec, refuse,
    validate_and_sanitize_html_template, validate_spec, DocumentKind, SourceFormat, TemplateSpec,
};

/// Output format an agent authors against. Descriptive only: there is no
/// renderer per format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthoringFormat {
    Docx,
    Pptx,
    Xlsx,
    Html,
    Text,
}

/// The only authoring kind. It is not a quotation, invoice, or proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthoringKind {
    Authoring,
}

/// One immutable reference file. `content_base64` is the bytes themselves;
/// the daemon never resolves a path or URL from this record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceAsset {
    pub name: String,
    pub media_type: String,
    pub description: String,
    pub content_base64: String,
}

/// A reference pack: guidelines plus the visual/textual references an
/// agent should learn from. Data only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoringSpec {
    pub kind: AuthoringKind,
    pub purpose: String,
    pub format: AuthoringFormat,
    pub guidelines: String,
    #[serde(default)]
    pub references: Vec<ReferenceAsset>,
}

/// One template body. Financial JSON is the historical spec object;
/// authoring JSON is a separate shape discriminated by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TemplateDefinition {
    Financial(TemplateSpec),
    Authoring(AuthoringSpec),
}

impl TemplateDefinition {
    /// The financial spec, or a refusal when this version is an authoring pack.
    pub fn financial(&self) -> Result<&TemplateSpec> {
        match self {
            Self::Financial(spec) => Ok(spec),
            Self::Authoring(_) => {
                refuse("this template is an authoring reference pack, not a financial document")
            }
        }
    }

    pub fn authoring(&self) -> Option<&AuthoringSpec> {
        match self {
            Self::Authoring(spec) => Some(spec),
            Self::Financial(_) => None,
        }
    }

    /// `quotation`, `invoice`, or `authoring`.
    pub fn kind(&self) -> &str {
        match self {
            Self::Financial(spec) => match spec.kind {
                DocumentKind::Quotation => "quotation",
                DocumentKind::Invoice => "invoice",
            },
            Self::Authoring(_) => "authoring",
        }
    }
}

/// Reference basenames the show export owns. A pack must not reuse them.
pub const RESERVED_REFERENCE_NAMES: &[&str] = &["guidelines.md", "template.json"];

pub(super) const PACK_MAX_REFERENCES: usize = 12;
const PACK_MAX_GUIDELINES: usize = 64 * 1024;
const PACK_MIN_GUIDELINES: usize = 16;
const PACK_MAX_PURPOSE: usize = 2_000;
const PACK_MIN_PURPOSE: usize = 8;
const PACK_MAX_DESCRIPTION: usize = 2_000;
const PACK_MIN_DESCRIPTION: usize = 8;
const PACK_MAX_NAME: usize = 120;
pub(super) const PACK_MAX_ASSET_BYTES: usize = 2 * 1024 * 1024;
const PACK_MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;

/// Parse and validate either a financial spec or an authoring pack.
/// Reference payload size is bounded before any base64 decode.
pub fn parse_definition(value: Value) -> Result<TemplateDefinition> {
    bound_authoring_wire(&value)?;
    let kind = value.get("kind").and_then(Value::as_str);
    match kind {
        Some("quotation") | Some("invoice") => {
            Ok(TemplateDefinition::Financial(parse_spec(value)?))
        }
        Some("authoring") => {
            let spec: AuthoringSpec = serde_json::from_value(value)
                .map_err(|e| Error::rejected(format!("authoring pack is malformed: {e}")))?;
            validate_authoring(&spec)?;
            Ok(TemplateDefinition::Authoring(spec))
        }
        Some(other) => refuse(format!(
            "template kind '{other}' is not quotation, invoice, or authoring"
        )),
        None => refuse("template spec is malformed: missing kind"),
    }
}

/// Structural validation for whichever body this definition carries.
pub fn validate_definition(definition: &TemplateDefinition) -> Result<()> {
    match definition {
        TemplateDefinition::Financial(spec) => validate_spec(spec),
        TemplateDefinition::Authoring(spec) => validate_authoring(spec),
    }
}

/// Financial versions still need confirmed calculation rules. A structurally
/// valid authoring pack may be approved by the operator; it has no prices.
pub fn check_definition_approvable(definition: &TemplateDefinition) -> Result<()> {
    match definition {
        TemplateDefinition::Financial(spec) => check_approvable(spec),
        TemplateDefinition::Authoring(spec) => validate_authoring(spec),
    }
}

/// The bytes retained for this definition. Financial versions keep the
/// checked DOCX. Authoring versions keep an empty blob and refuse a DOCX.
pub fn retained_source<'a>(definition: &TemplateDefinition, source: &'a [u8]) -> Result<&'a [u8]> {
    match definition {
        TemplateDefinition::Financial(_) => {
            check_docx_package(source)?;
            Ok(source)
        }
        TemplateDefinition::Authoring(_) => {
            if !source.is_empty() {
                return refuse("authoring reference packs do not accept a DOCX source");
            }
            Ok(source)
        }
    }
}

/// Format-aware source validation and canonicalization for stored template versions.
pub fn retained_source_with_format(
    definition: &TemplateDefinition,
    format: SourceFormat,
    source: &[u8],
) -> Result<Vec<u8>> {
    match (definition, format) {
        (TemplateDefinition::Financial(_), SourceFormat::Docx) => {
            check_docx_package(source)?;
            Ok(source.to_vec())
        }
        (TemplateDefinition::Financial(spec), SourceFormat::Html) => {
            let clean = validate_and_sanitize_html_template(source, spec)?;
            Ok(clean.into_bytes())
        }
        (TemplateDefinition::Financial(_), SourceFormat::Authoring) => {
            refuse("financial templates require docx or html source format")
        }
        (TemplateDefinition::Authoring(_), SourceFormat::Authoring) => {
            if !source.is_empty() {
                return refuse("authoring reference packs do not accept a source payload");
            }
            Ok(Vec::new())
        }
        (TemplateDefinition::Authoring(_), _) => {
            refuse("authoring reference packs only accept authoring source format")
        }
    }
}

fn bound_authoring_wire(value: &Value) -> Result<()> {
    if value.get("kind").and_then(Value::as_str) != Some("authoring") {
        return Ok(());
    }
    if let Some(guidelines) = value.get("guidelines").and_then(Value::as_str) {
        if guidelines.len() > PACK_MAX_GUIDELINES {
            return refuse(format!("guidelines exceed {PACK_MAX_GUIDELINES} bytes"));
        }
    }
    let Some(refs) = value.get("references") else {
        return Ok(());
    };
    let Some(refs) = refs.as_array() else {
        return Ok(());
    };
    if refs.len() > PACK_MAX_REFERENCES {
        return refuse(format!("at most {PACK_MAX_REFERENCES} reference assets"));
    }
    let mut wire = 0usize;
    for reference in refs {
        let encoded = reference
            .get("content_base64")
            .and_then(Value::as_str)
            .unwrap_or("");
        let asset_limit = PACK_MAX_ASSET_BYTES * 4 / 3 + 4;
        if encoded.len() > asset_limit {
            return refuse(format!(
                "a reference exceeds {PACK_MAX_ASSET_BYTES} decoded bytes"
            ));
        }
        wire = wire.saturating_add(encoded.len());
    }
    let total_limit = PACK_MAX_TOTAL_BYTES * 4 / 3 + refs.len() * 4;
    if wire > total_limit {
        return refuse(format!(
            "reference assets exceed {PACK_MAX_TOTAL_BYTES} decoded bytes in total"
        ));
    }
    Ok(())
}

fn validate_authoring(spec: &AuthoringSpec) -> Result<()> {
    let purpose = spec.purpose.trim();
    if purpose.len() < PACK_MIN_PURPOSE || spec.purpose.len() > PACK_MAX_PURPOSE {
        return refuse(format!(
            "purpose must be {PACK_MIN_PURPOSE}..={PACK_MAX_PURPOSE} bytes of text"
        ));
    }
    let guidelines = spec.guidelines.trim();
    if guidelines.len() < PACK_MIN_GUIDELINES || spec.guidelines.len() > PACK_MAX_GUIDELINES {
        return refuse(format!(
            "guidelines must be {PACK_MIN_GUIDELINES}..={PACK_MAX_GUIDELINES} bytes"
        ));
    }
    if spec.references.len() > PACK_MAX_REFERENCES {
        return refuse(format!("at most {PACK_MAX_REFERENCES} reference assets"));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut total = 0usize;
    for reference in &spec.references {
        check_reference_name(&reference.name)?;
        let key = reference.name.to_ascii_lowercase();
        if !seen.insert(key) {
            return refuse(format!("reference '{}' is declared twice", reference.name));
        }
        if !reference_extension_matches(&reference.name, &reference.media_type)? {
            return refuse(format!(
                "reference '{}' extension does not match media_type '{}'",
                reference.name, reference.media_type
            ));
        }
        let description = reference.description.trim();
        if description.len() < PACK_MIN_DESCRIPTION
            || reference.description.len() > PACK_MAX_DESCRIPTION
        {
            return refuse(format!(
                "reference '{}' needs a description of {PACK_MIN_DESCRIPTION}..={PACK_MAX_DESCRIPTION} bytes",
                reference.name
            ));
        }
        let asset_limit = PACK_MAX_ASSET_BYTES * 4 / 3 + 4;
        if reference.content_base64.len() > asset_limit {
            return refuse(format!(
                "reference '{}' exceeds {PACK_MAX_ASSET_BYTES} decoded bytes",
                reference.name
            ));
        }
        let bytes = decode_reference(&reference.content_base64).map_err(|_| {
            Error::rejected(format!(
                "reference '{}' content_base64 is not standard base64",
                reference.name
            ))
        })?;
        if bytes.len() > PACK_MAX_ASSET_BYTES {
            return refuse(format!(
                "reference '{}' exceeds {PACK_MAX_ASSET_BYTES} decoded bytes",
                reference.name
            ));
        }
        total = total.saturating_add(bytes.len());
        if total > PACK_MAX_TOTAL_BYTES {
            return refuse(format!(
                "reference assets exceed {PACK_MAX_TOTAL_BYTES} decoded bytes in total"
            ));
        }
        check_reference_bytes(&reference.name, &reference.media_type, &bytes)?;
    }
    Ok(())
}

fn check_reference_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= PACK_MAX_NAME
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        && !name.contains('/')
        && !name.contains('\\');
    if !ok {
        return refuse(format!(
            "reference name '{name}' must be a flat basename of letters, digits, '.', '_' or '-'"
        ));
    }
    if RESERVED_REFERENCE_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
    {
        return refuse(format!("reference name '{name}' is reserved"));
    }
    Ok(())
}

fn decode_reference(text: &str) -> std::result::Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(text)
}

/// The basename extension must be one of the allowlisted suffixes for
/// `media_type`. Comparison is ASCII case-insensitive. Returns an error
/// when `media_type` itself is not an allowlisted type.
fn reference_extension_matches(name: &str, media_type: &str) -> Result<bool> {
    let ext = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    let allowed: &[&str] = match media_type {
        "image/png" => &["png"],
        "image/jpeg" => &["jpg", "jpeg"],
        "application/pdf" => &["pdf"],
        "text/plain" => &["txt"],
        "text/markdown" => &["md"],
        "text/html" => &["html", "htm"],
        other => {
            return refuse(format!(
                "reference '{name}' media_type '{other}' is not png, jpeg, pdf, plain text, markdown, or html"
            ))
        }
    };
    Ok(allowed
        .iter()
        .any(|suffix| suffix.eq_ignore_ascii_case(ext)))
}

fn check_reference_bytes(name: &str, media_type: &str, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return refuse(format!("reference '{name}' is empty"));
    }
    let ok = match media_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "text/plain" | "text/markdown" | "text/html" => text_bytes(bytes),
        _ => false,
    };
    if !ok {
        return refuse(format!(
            "reference '{name}' does not match media_type '{media_type}'"
        ));
    }
    if media_type == "text/html" {
        html::validate_html_reference(bytes)?;
    }
    Ok(())
}

/// UTF-8 text. The only refused controls are bytes below 0x20 other than
/// tab, LF, and CR. Contents stay untrusted data; nothing here is executed.
fn text_bytes(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    text.bytes()
        .all(|b| b >= 0x20 || b == b'\t' || b == b'\n' || b == b'\r')
}
