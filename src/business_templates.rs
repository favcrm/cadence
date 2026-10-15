//! CAD-1393: approved company document templates — the pure model.
//!
//! A template is a validated spec: named variable fields, fixed business
//! text, one line-item table and explicitly confirmed calculation rules.
//! This module does no I/O. The store persists specs and the RPC layer
//! decides who may act; here we only validate, digest and calculate.
//!
//! Money is integer minor units and quantities are integer thousandths,
//! so every total is exact. Rounding is half-up to the minor unit, applied
//! to each line, the discount and the tax. No model arithmetic exists.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Largest accepted money amount in minor units (10^13 cents).
const MAX_MINOR: i128 = 10_000_000_000_000;
/// Largest accepted quantity in thousandths (10^12 milli-units).
const MAX_MILLI: i128 = 1_000_000_000_000;
const MAX_FIELDS: usize = 50;
const MAX_FIXED_BLOCKS: usize = 20;
const MAX_ITEMS: usize = 200;
const MAX_TEXT: usize = 2_000;
const MAX_FIXED_TEXT: usize = 4_000;
/// Names the draft context owns; a field or fixed block may not use one.
const RESERVED: &[&str] = &[
    "items",
    "subtotal",
    "discount",
    "tax",
    "total",
    "currency",
    "tax_rate",
    "discount_rate",
    "kind",
];

/// The document kind a template produces. Drafts only: no kind issues,
/// numbers or sends anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentKind {
    Quotation,
    Invoice,
}

/// The type of one variable field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Date,
    Money,
    Number,
}

/// One variable scalar field (customer name, date, terms, ...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSpec {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub required: bool,
}

/// The confirmed calculation rules. `confirmed` must be true before a
/// version can be approved or used for a draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalcRules {
    pub confirmed: bool,
    /// ISO 4217 code, three ASCII capitals. Amounts use two minor digits.
    pub currency: String,
    /// Tax rate in basis points (1000 = 10.00%), 0..=10000.
    pub tax_rate_bps: u32,
    /// Document discount in basis points, 0..=10000, taken before tax.
    pub discount_bps: u32,
}

/// A template spec: the content a version approves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateSpec {
    pub kind: DocumentKind,
    pub fields: Vec<FieldSpec>,
    /// Fixed business text (for example payment terms), keyed by name.
    #[serde(default)]
    pub fixed_text: BTreeMap<String, String>,
    pub calculation: CalcRules,
}

/// One line-item row as supplied: quantities and prices are exact decimal
/// text, never JSON numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineItem {
    pub description: String,
    pub quantity: String,
    pub unit_price: String,
}

/// The structured data for one draft.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentData {
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    #[serde(default)]
    pub items: Vec<LineItem>,
}

/// One calculated line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemOut {
    pub description: String,
    pub quantity: String,
    pub unit_price: String,
    pub line_total: String,
}

/// A validated, calculated draft. `context()` is the complete placeholder
/// map a later DOCX render reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub kind: DocumentKind,
    pub fields: BTreeMap<String, String>,
    pub fixed_text: BTreeMap<String, String>,
    pub items: Vec<ItemOut>,
    pub currency: String,
    pub tax_rate_bps: u32,
    pub discount_bps: u32,
    pub subtotal: String,
    pub discount: String,
    pub tax: String,
    pub total: String,
}

impl Draft {
    /// The placeholder context for rendering.
    pub fn context(&self) -> Value {
        let mut ctx = serde_json::Map::new();
        for (k, v) in self.fixed_text.iter().chain(self.fields.iter()) {
            ctx.insert(k.clone(), json!(v));
        }
        ctx.insert("kind".into(), json!(self.kind));
        ctx.insert("currency".into(), json!(self.currency));
        ctx.insert("tax_rate".into(), json!(percent(self.tax_rate_bps)));
        ctx.insert("discount_rate".into(), json!(percent(self.discount_bps)));
        ctx.insert("subtotal".into(), json!(self.subtotal));
        ctx.insert("discount".into(), json!(self.discount));
        ctx.insert("tax".into(), json!(self.tax));
        ctx.insert("total".into(), json!(self.total));
        ctx.insert("items".into(), json!(self.items));
        Value::Object(ctx)
    }
}

fn refuse<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::rejected(message))
}

/// Parse and validate a template spec from JSON (the single entry point).
pub fn parse_spec(value: Value) -> Result<TemplateSpec> {
    let spec: TemplateSpec = serde_json::from_value(value)
        .map_err(|e| Error::rejected(format!("template spec is malformed: {e}")))?;
    validate_spec(&spec)?;
    Ok(spec)
}

/// Structural validation: keys, bounds, reserved names and rule ranges.
pub fn validate_spec(spec: &TemplateSpec) -> Result<()> {
    if spec.fields.is_empty() || spec.fields.len() > MAX_FIELDS {
        return refuse(format!("a template needs 1..={MAX_FIELDS} fields"));
    }
    if spec.fixed_text.len() > MAX_FIXED_BLOCKS {
        return refuse(format!("at most {MAX_FIXED_BLOCKS} fixed text blocks"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for field in &spec.fields {
        check_key(&field.key)?;
        if field.label.trim().is_empty() || field.label.len() > 200 {
            return refuse(format!(
                "field '{}' needs a label of 1..=200 bytes",
                field.key
            ));
        }
        if !seen.insert(field.key.as_str()) {
            return refuse(format!("field '{}' is declared twice", field.key));
        }
    }
    for (key, text) in &spec.fixed_text {
        check_key(key)?;
        if !seen.insert(key.as_str()) {
            return refuse(format!("fixed text '{key}' collides with a field"));
        }
        if text.len() > MAX_FIXED_TEXT {
            return refuse(format!("fixed text '{key}' exceeds {MAX_FIXED_TEXT} bytes"));
        }
    }
    let rules = &spec.calculation;
    if rules.currency.len() != 3 || !rules.currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return refuse("currency must be a three-letter ISO code");
    }
    if rules.tax_rate_bps > 10_000 || rules.discount_bps > 10_000 {
        return refuse("tax and discount rates are 0..=10000 basis points");
    }
    Ok(())
}

fn check_key(key: &str) -> Result<()> {
    let ok = !key.is_empty()
        && key.len() <= 64
        && key.as_bytes()[0].is_ascii_lowercase()
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !ok {
        return refuse(format!(
            "key '{key}' must be lowercase letters, digits or _"
        ));
    }
    if RESERVED.contains(&key) {
        return refuse(format!("key '{key}' is reserved"));
    }
    Ok(())
}

/// Largest accepted DOCX source in bytes.
pub const DOCX_MAX_BYTES: usize = 10 * 1024 * 1024;
const DOCX_MAX_ENTRIES: usize = 1_000;

/// Lowercase hex SHA-256 of raw bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// The version digest a human approves: the retained source bytes and
/// the canonical definition, both. A financial definition serializes as
/// the historical `TemplateSpec` JSON, so existing digests stay put.
/// An authoring pack's guidelines and reference bytes live in that same
/// JSON, so the digest covers the whole pack. Changing either input
/// changes the digest.
pub fn content_digest(source_sha256: &str, definition: &TemplateDefinition) -> Result<String> {
    let spec_json =
        serde_json::to_string(definition).map_err(|e| Error::internal(e.to_string()))?;
    Ok(sha256_hex(
        format!("cad1393-template-v1\n{source_sha256}\n{spec_json}").as_bytes(),
    ))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Structural check of a DOCX source (a ZIP package). It reads only the
/// central directory and never inflates an entry, so a hostile package
/// costs bounded work. Refused: oversize, non-ZIP, ZIP64, encrypted,
/// traversal or absolute entry names, macro-enabled parts, and any
/// package without `word/document.xml`.
pub fn check_docx_package(bytes: &[u8]) -> Result<()> {
    if bytes.len() > DOCX_MAX_BYTES {
        return refuse(format!("DOCX source exceeds {DOCX_MAX_BYTES} bytes"));
    }
    if bytes.len() < 22 || !bytes.starts_with(b"PK\x03\x04") {
        return refuse("source is not a DOCX package (ZIP)");
    }
    let eocd = find_eocd(bytes)
        .ok_or_else(|| Error::rejected("DOCX package has no end-of-central-directory record"))?;
    let count = u16le(bytes, eocd + 10) as usize;
    let cd_size = u32le(bytes, eocd + 12) as usize;
    let cd_offset = u32le(bytes, eocd + 16) as usize;
    if count == 0xffff || cd_offset == 0xffff_ffff {
        return refuse("ZIP64 DOCX packages are not accepted");
    }
    if count > DOCX_MAX_ENTRIES || cd_offset.checked_add(cd_size).is_none_or(|end| end > eocd) {
        return refuse("DOCX package directory is malformed or too large");
    }
    let mut names: Vec<String> = Vec::with_capacity(count);
    let mut pos = cd_offset;
    for _ in 0..count {
        if pos + 46 > eocd || bytes.get(pos..pos + 4) != Some(b"PK\x01\x02") {
            return refuse("DOCX package directory is malformed");
        }
        if u16le(bytes, pos + 8) & 1 != 0 {
            return refuse("encrypted DOCX packages are not accepted");
        }
        let name_len = u16le(bytes, pos + 28) as usize;
        let extra_len = u16le(bytes, pos + 30) as usize;
        let comment_len = u16le(bytes, pos + 32) as usize;
        let start = pos + 46;
        let Some(name) = bytes.get(start..start + name_len) else {
            return refuse("DOCX package directory is malformed");
        };
        let name = std::str::from_utf8(name)
            .map_err(|_| Error::rejected("DOCX entry name is not UTF-8"))?;
        if name.starts_with('/') || name.contains('\\') || name.split('/').any(|p| p == "..") {
            return refuse(format!("DOCX entry '{name}' has an unsafe path"));
        }
        names.push(name.to_string());
        pos = start + name_len + extra_len + comment_len;
    }
    if names.iter().any(|n| n == "word/vbaProject.bin") {
        return refuse("macro-enabled DOCX packages are not accepted");
    }
    if !names.iter().any(|n| n == "word/document.xml")
        || !names.iter().any(|n| n == "[Content_Types].xml")
    {
        return refuse("source is not a DOCX package: word/document.xml is missing");
    }
    Ok(())
}

fn find_eocd(bytes: &[u8]) -> Option<usize> {
    let last = bytes.len().checked_sub(22)?;
    let first = last.saturating_sub(65_535);
    (first..=last)
        .rev()
        .find(|&i| bytes[i..].starts_with(b"PK\x05\x06"))
}

fn u16le(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32le(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Human approval is refused unless the calculation rules are confirmed.
pub fn check_approvable(spec: &TemplateSpec) -> Result<()> {
    if !spec.calculation.confirmed {
        return refuse("calculation rules are not confirmed; confirm them before approval");
    }
    Ok(())
}

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

const PACK_MAX_REFERENCES: usize = 12;
const PACK_MAX_GUIDELINES: usize = 64 * 1024;
const PACK_MIN_GUIDELINES: usize = 16;
const PACK_MAX_PURPOSE: usize = 2_000;
const PACK_MIN_PURPOSE: usize = 8;
const PACK_MAX_DESCRIPTION: usize = 2_000;
const PACK_MIN_DESCRIPTION: usize = 8;
const PACK_MAX_NAME: usize = 120;
const PACK_MAX_ASSET_BYTES: usize = 2 * 1024 * 1024;
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
        other => {
            return refuse(format!(
                "reference '{name}' media_type '{other}' is not png, jpeg, pdf, plain text, or markdown"
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
        "text/plain" | "text/markdown" => text_bytes(bytes),
        _ => false,
    };
    if !ok {
        return refuse(format!(
            "reference '{name}' does not match media_type '{media_type}'"
        ));
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

/// Placeholder data that exercises every declared field and one line
/// item, for a labelled sample preview. It invents no company facts: every
/// value is a fixed marker, and the sample is never stored as a document.
pub fn sample_data(spec: &TemplateSpec) -> DocumentData {
    let fields = spec
        .fields
        .iter()
        .map(|f| {
            let value = match f.kind {
                FieldKind::Text => format!("SAMPLE {}", f.label),
                FieldKind::Date => "2000-01-01".to_string(),
                FieldKind::Money => "1.00".to_string(),
                FieldKind::Number => "1".to_string(),
            };
            (f.key.clone(), value)
        })
        .collect();
    DocumentData {
        fields,
        items: vec![LineItem {
            description: "SAMPLE line item".to_string(),
            quantity: "1".to_string(),
            unit_price: "1.00".to_string(),
        }],
    }
}

/// Validate `data` against `spec` and compute every total in code.
pub fn build_draft(spec: &TemplateSpec, data: &DocumentData) -> Result<Draft> {
    validate_spec(spec)?;
    check_approvable(spec)?;
    for key in data.fields.keys() {
        if !spec.fields.iter().any(|f| &f.key == key) {
            return refuse(format!("field '{key}' is not declared by this template"));
        }
    }
    let mut fields = BTreeMap::new();
    for field in &spec.fields {
        let raw = data.fields.get(&field.key).map(|v| v.trim()).unwrap_or("");
        if raw.is_empty() {
            if field.required {
                return refuse(format!("required field '{}' is missing", field.key));
            }
            fields.insert(field.key.clone(), String::new());
            continue;
        }
        let normalized = match field.kind {
            FieldKind::Text => {
                if raw.len() > MAX_TEXT {
                    return refuse(format!("field '{}' exceeds {MAX_TEXT} bytes", field.key));
                }
                raw.to_string()
            }
            FieldKind::Date => {
                parse_date(raw)?;
                raw.to_string()
            }
            FieldKind::Money => format_minor(parse_money(raw)?),
            FieldKind::Number => format_milli(parse_quantity(raw)?),
        };
        fields.insert(field.key.clone(), normalized);
    }
    if data.items.is_empty() {
        return refuse("at least one line item is required");
    }
    if data.items.len() > MAX_ITEMS {
        return refuse(format!("at most {MAX_ITEMS} line items"));
    }
    let rules = &spec.calculation;
    let mut items = Vec::with_capacity(data.items.len());
    let mut subtotal: i128 = 0;
    for (index, row) in data.items.iter().enumerate() {
        let n = index + 1;
        let description = row.description.trim();
        if description.is_empty() || description.len() > MAX_TEXT {
            return refuse(format!(
                "line item {n} needs a description of 1..={MAX_TEXT} bytes"
            ));
        }
        let qty = parse_quantity(&row.quantity)
            .map_err(|e| Error::rejected(format!("line item {n}: {e}")))?;
        let price = parse_money(&row.unit_price)
            .map_err(|e| Error::rejected(format!("line item {n}: {e}")))?;
        let line = round_div(qty * price, 1000);
        subtotal += line;
        items.push(ItemOut {
            description: description.to_string(),
            quantity: format_milli(qty),
            unit_price: format_minor(price),
            line_total: format_minor(line),
        });
    }
    let discount = round_div(subtotal * rules.discount_bps as i128, 10_000);
    let taxable = subtotal - discount;
    let tax = round_div(taxable * rules.tax_rate_bps as i128, 10_000);
    let total = taxable + tax;
    Ok(Draft {
        kind: spec.kind,
        fields,
        fixed_text: spec.fixed_text.clone(),
        items,
        currency: rules.currency.clone(),
        tax_rate_bps: rules.tax_rate_bps,
        discount_bps: rules.discount_bps,
        subtotal: format_minor(subtotal),
        discount: format_minor(discount),
        tax: format_minor(tax),
        total: format_minor(total),
    })
}

/// Half-up division of a non-negative numerator, to the nearest integer.
fn round_div(num: i128, den: i128) -> i128 {
    (2 * num + den) / (2 * den)
}

/// Parse `1234`, `1234.5` or `1234.56` into minor units (no sign, no
/// separators, at most two decimals).
pub fn parse_money(text: &str) -> Result<i128> {
    let (whole, frac) = split_decimal(text, 2, "amount")?;
    let minor = whole * 100 + frac;
    if minor > MAX_MINOR {
        return refuse(format!("amount '{text}' is too large"));
    }
    Ok(minor)
}

/// Parse a quantity into thousandths (at most three decimals, positive).
pub fn parse_quantity(text: &str) -> Result<i128> {
    let (whole, frac) = split_decimal(text, 3, "quantity")?;
    let milli = whole * 1000 + frac;
    if milli <= 0 {
        return refuse(format!("quantity '{text}' must be positive"));
    }
    if milli > MAX_MILLI {
        return refuse(format!("quantity '{text}' is too large"));
    }
    Ok(milli)
}

/// `(whole, fraction scaled to `digits`)` for an unsigned decimal string.
fn split_decimal(text: &str, digits: usize, what: &str) -> Result<(i128, i128)> {
    let (whole, frac) = match text.split_once('.') {
        Some((w, f)) => (w, f),
        None => (text, ""),
    };
    let ok = !whole.is_empty()
        && whole.len() <= 15
        && whole.bytes().all(|b| b.is_ascii_digit())
        && frac.len() <= digits
        && frac.bytes().all(|b| b.is_ascii_digit());
    if !ok || (text.contains('.') && frac.is_empty()) {
        return refuse(format!(
            "{what} '{text}' must be an unsigned decimal with at most {digits} decimals"
        ));
    }
    let whole: i128 = whole.parse().map_err(|_| Error::rejected("bad number"))?;
    let mut padded = frac.to_string();
    while padded.len() < digits {
        padded.push('0');
    }
    let frac: i128 = if digits == 0 {
        0
    } else {
        padded.parse().map_err(|_| Error::rejected("bad number"))?
    };
    Ok((whole, frac))
}

fn format_minor(minor: i128) -> String {
    format!("{}.{:02}", minor / 100, minor % 100)
}

fn format_milli(milli: i128) -> String {
    format!("{}.{:03}", milli / 1000, milli % 1000)
}

fn percent(bps: u32) -> String {
    format!("{}.{:02}", bps / 100, bps % 100)
}

/// A strict `YYYY-MM-DD` calendar date between 1900 and 2999.
fn parse_date(text: &str) -> Result<()> {
    let parts: Vec<&str> = text.split('-').collect();
    let well_formed = text.len() == 10
        && parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && text.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    if !well_formed {
        return refuse(format!("date '{text}' must be YYYY-MM-DD"));
    }
    let year: u32 = parts[0].parse().unwrap_or(0);
    let month: u32 = parts[1].parse().unwrap_or(0);
    let day: u32 = parts[2].parse().unwrap_or(0);
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if !(1900..=2999).contains(&year) || day == 0 || day > days {
        return refuse(format!("date '{text}' is not a valid calendar date"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> TemplateSpec {
        parse_spec(json!({
            "kind": "quotation",
            "fields": [
                {"key": "customer", "label": "Customer", "kind": "text", "required": true},
                {"key": "valid_until", "label": "Valid until", "kind": "date", "required": false}
            ],
            "fixed_text": {"terms": "Payment within 30 days"},
            "calculation": {"confirmed": true, "currency": "EUR", "tax_rate_bps": 2000, "discount_bps": 1000}
        }))
        .unwrap()
    }

    fn data(items: Value) -> DocumentData {
        serde_json::from_value(json!({"fields": {"customer": "Acme"}, "items": items})).unwrap()
    }

    #[test]
    fn totals_are_exact_and_round_half_up() {
        let draft = build_draft(
            &spec(),
            &data(json!([
                {"description": "A", "quantity": "2", "unit_price": "10"},
                {"description": "B", "quantity": "1.5", "unit_price": "3.33"}
            ])),
        )
        .unwrap();
        assert_eq!(draft.items[1].line_total, "5.00");
        assert_eq!(draft.subtotal, "25.00");
        assert_eq!(draft.discount, "2.50");
        assert_eq!(draft.tax, "4.50");
        assert_eq!(draft.total, "27.00");
        // 0.5 x 0.05 = 0.025 exact minor units -> 0.03 (half-up).
        let half = build_draft(
            &spec(),
            &data(json!([{"description": "H", "quantity": "0.5", "unit_price": "0.05"}])),
        )
        .unwrap();
        assert_eq!(half.items[0].line_total, "0.03");
    }

    #[test]
    fn refuses_bad_or_missing_input() {
        let row = |q: &str, p: &str| json!([{"description": "x", "quantity": q, "unit_price": p}]);
        assert!(build_draft(&spec(), &DocumentData::default()).is_err());
        assert!(build_draft(&spec(), &data(row("1", "1,000"))).is_err());
        assert!(build_draft(&spec(), &data(row("1", "1.005"))).is_err());
        assert!(build_draft(&spec(), &data(row("0", "1"))).is_err());
        let undeclared: DocumentData = serde_json::from_value(
            json!({"fields": {"customer": "A", "x": "y"}, "items": row("1", "1")}),
        )
        .unwrap();
        assert!(build_draft(&spec(), &undeclared).is_err());
        let no_customer: DocumentData =
            serde_json::from_value(json!({"items": row("1", "1")})).unwrap();
        assert!(build_draft(&spec(), &no_customer)
            .unwrap_err()
            .to_string()
            .contains("customer"));
        let bad_date: DocumentData = serde_json::from_value(json!({"fields": {"customer": "A", "valid_until": "2026-02-30"}, "items": row("1", "1")})).unwrap();
        assert!(build_draft(&spec(), &bad_date).is_err());
    }

    #[test]
    fn refuses_unconfirmed_rules_and_reserved_keys() {
        let mut unconfirmed = spec();
        unconfirmed.calculation.confirmed = false;
        assert!(build_draft(
            &unconfirmed,
            &data(json!([{"description": "x", "quantity": "1", "unit_price": "1"}]))
        )
        .is_err());
        let reserved = json!({
            "kind": "invoice",
            "fields": [{"key": "total", "label": "T", "kind": "text", "required": false}],
            "calculation": {"confirmed": true, "currency": "EUR", "tax_rate_bps": 0, "discount_bps": 0}
        });
        assert!(parse_spec(reserved).is_err());
    }

    fn financial(spec: &TemplateSpec) -> TemplateDefinition {
        TemplateDefinition::Financial(spec.clone())
    }

    #[test]
    fn digest_binds_source_and_spec() {
        let a = spec();
        let mut b = spec();
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&financial(&a)).unwrap()
        );
        assert_eq!(
            content_digest("s1", &financial(&a)).unwrap(),
            content_digest("s1", &financial(&b)).unwrap()
        );
        assert_ne!(
            content_digest("s1", &financial(&a)).unwrap(),
            content_digest("s2", &financial(&a)).unwrap()
        );
        b.fixed_text.insert("terms".into(), "Net 60".into());
        assert_ne!(
            content_digest("s1", &financial(&a)).unwrap(),
            content_digest("s1", &financial(&b)).unwrap()
        );
    }

    fn pack(references: Value) -> Value {
        json!({
            "kind": "authoring",
            "purpose": "Cover letters for fictional bids",
            "format": "docx",
            "guidelines": "Use a single left-aligned column and the sample heading scale.",
            "references": references
        })
    }

    fn png_ref() -> Value {
        json!({
            "name": "cover.png",
            "media_type": "image/png",
            "description": "Sample cover crop showing the heading scale.",
            "content_base64": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVQI12P4z8AAAAADAAEABf4C/gAAAABJRU5ErkJggg=="
        })
    }

    #[test]
    fn authoring_pack_digest_covers_guidelines_and_rejects_financial_use() {
        let parsed = parse_definition(pack(json!([png_ref()]))).unwrap();
        assert!(parsed.authoring().is_some());
        assert_eq!(parsed.kind(), "authoring");
        assert!(parsed.financial().is_err());
        assert!(check_definition_approvable(&parsed).is_ok());
        assert!(retained_source(&parsed, b"").is_ok());
        assert!(retained_source(&parsed, b"PK").is_err());
        let mut changed = parsed.authoring().unwrap().clone();
        let original = content_digest(&sha256_hex(b""), &parsed).unwrap();
        changed
            .guidelines
            .push_str(" Prefer the sample's left margin.");
        let next = TemplateDefinition::Authoring(changed);
        assert_ne!(original, content_digest(&sha256_hex(b""), &next).unwrap());
        let round = serde_json::to_string(&parsed).unwrap();
        let again = parse_definition(serde_json::from_str(&round).unwrap()).unwrap();
        assert_eq!(original, content_digest(&sha256_hex(b""), &again).unwrap());
    }

    #[test]
    fn authoring_pack_rejects_unsafe_or_oversized_references() {
        assert!(parse_definition(json!({
            "kind": "proposal",
            "purpose": "not a real kind",
            "format": "docx",
            "guidelines": "This must not become a document kind."
        }))
        .unwrap_err()
        .to_string()
        .contains("not quotation, invoice, or authoring"));
        let bad_name = png_ref();
        for (name, needle) in [
            ("guidelines.md", "reserved"),
            ("template.json", "reserved"),
            ("../cover.png", "flat basename"),
            ("nested/cover.png", "flat basename"),
        ] {
            let mut asset = bad_name.clone();
            asset["name"] = json!(name);
            let err = parse_definition(pack(json!([asset])))
                .unwrap_err()
                .to_string();
            assert!(err.contains(needle), "{name}: {err}");
        }
        let mut html = png_ref();
        html["name"] = json!("script.html");
        html["media_type"] = json!("text/plain");
        html["content_base64"] = json!(base64_std(b"not really html\n"));
        assert!(parse_definition(pack(json!([html]))).is_err());
        let notes = json!({
            "name": "notes.md",
            "media_type": "text/markdown",
            "description": "Notes that may quote prohibited patterns.",
            "content_base64": base64_std(b"Do not copy a <script> tag or javascript: URL.\n")
        });
        assert!(parse_definition(pack(json!([notes]))).is_ok());
        let layout = json!({
            "name": "layout.txt",
            "media_type": "text/plain",
            "description": "Column and heading measurements.",
            "content_base64": base64_std(b"Left margin 18mm. Heading 18pt.\n")
        });
        assert!(parse_definition(pack(json!([layout]))).is_ok());
        let mut lie = png_ref();
        lie["media_type"] = json!("application/pdf");
        assert!(parse_definition(pack(json!([lie]))).is_err());
        let mut extra = pack(json!([png_ref()]));
        extra["remote"] = json!("https://example.invalid/x.png");
        assert!(parse_definition(extra)
            .unwrap_err()
            .to_string()
            .contains("malformed"));
        let mut huge = png_ref();
        huge["content_base64"] = json!("A".repeat(PACK_MAX_ASSET_BYTES * 4 / 3 + 8));
        assert!(parse_definition(pack(json!([huge]))).is_err());
        let many: Vec<_> = (0..PACK_MAX_REFERENCES + 1).map(|_| png_ref()).collect();
        assert!(parse_definition(pack(json!(many))).is_err());
    }

    fn base64_std(bytes: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }
}
