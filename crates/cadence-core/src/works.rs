//! Works content domain, shared by the native store and hosted adapters.
//!
//! The grammar is deliberately small and closed: a `doc`'s blocks, a `deck`'s
//! slides and a `sheet`'s columns/rows, with unknown kinds, fields, block
//! types and malformed or oversize payloads refused before any write. The
//! bounds, messages and review/export rules are the ones the native store has
//! always enforced; the store calls this module instead of keeping its own
//! copy, and a host evaluates the same rules through `evaluate`'s `works.*`
//! operations.
//!
//! Only domain input is judged here. Caller identity, the stored current
//! state, revision CAS, idempotency, nonces and audit stay host-side: the
//! host passes the state it has already proven, and no caller-shaped proof
//! flag can reach these functions.
use crate::material::material;
use serde_json::{json, Map, Value};

/// Longest title in UTF-8 bytes.
pub const TITLE_BYTES: usize = 240;
/// Longest encoded content payload in bytes.
pub const CONTENT_BYTES: usize = 64 * 1024;
/// Most tags one item may carry after trimming.
pub const TAGS_MAX: usize = 20;
/// Longest tag in UTF-8 bytes.
pub const TAG_BYTES: usize = 60;
/// Longest review note in UTF-8 bytes.
pub const NOTE_BYTES: usize = 2_000;
/// Longest export format in UTF-8 bytes.
pub const FORMAT_BYTES: usize = 40;
/// The closed set of Works kinds.
pub const KINDS: &[&str] = &["doc", "deck", "sheet"];

/// A refused domain operation, in the shape the native wire already carries:
/// `kind` is `rejected` (invalid input) or `conflict` (the caller must reload
/// rather than retry), `code` the stable short code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub kind: &'static str,
    pub code: &'static str,
    pub message: String,
}

fn invalid(code: &'static str, message: impl Into<String>) -> Violation {
    Violation {
        kind: "rejected",
        code,
        message: message.into(),
    }
}

/// A stale revision, or a review that is not pending for the named revision.
fn conflict(message: impl Into<String>) -> Violation {
    Violation {
        kind: "conflict",
        code: "conflict",
        message: message.into(),
    }
}

/// A parsed Works draft — the only content the store accepts.
#[derive(Clone, Debug)]
pub struct Draft {
    pub kind: String,
    pub title: String,
    pub content: Value,
    pub tags: Vec<String>,
}

impl Draft {
    /// Parse a create request: `kind`, `title`, `content` and optional `tags`.
    /// Field order and refusals match the native store exactly.
    pub fn parse(params: &Value) -> Result<Self, Violation> {
        let kind = required_str(params, "kind")?.to_string();
        let title = required_str(params, "title")?.trim().to_string();
        let content = params
            .get("content")
            .cloned()
            .ok_or_else(|| invalid("invalid", "works content is required"))?;
        let tags = match params.get("tags") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => {
                if items.len() > TAGS_MAX {
                    return Err(invalid("invalid", "works tags exceed their bound"));
                }
                let mut tags = Vec::with_capacity(items.len());
                for item in items {
                    let tag = item
                        .as_str()
                        .ok_or_else(|| invalid("invalid", "works tags must be strings"))?
                        .trim()
                        .to_string();
                    if tag.is_empty() || tag.len() > TAG_BYTES {
                        return Err(invalid(
                            "invalid",
                            "works tags must be non-empty and bounded",
                        ));
                    }
                    if !tags.contains(&tag) {
                        tags.push(tag);
                    }
                }
                tags
            }
            Some(_) => return Err(invalid("invalid", "works tags must be an array")),
        };
        if title.is_empty() || title.len() > TITLE_BYTES {
            return Err(invalid("invalid", "works title is required and bounded"));
        }
        validate_content(&kind, &content)?;
        Ok(Self {
            kind,
            title,
            content,
            tags,
        })
    }

    /// The digest material of kind, title and content — the bytes the native
    /// `contentDigest` hashes (`sha256:` + hex of their SHA-256).
    pub fn content_material(&self) -> String {
        content_material(&self.kind, &self.title, &self.content)
    }

    /// The digest material that also covers tags, used to pin idempotent
    /// replay to the exact payload.
    pub fn payload_material(&self) -> String {
        payload_material(&self.kind, &self.title, &self.content, &self.tags)
    }
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, Violation> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid("invalid", format!("{key} is required")))
}

/// The material a revision's `contentDigest` covers.
pub fn content_material(kind: &str, title: &str, content: &Value) -> String {
    material(&json!({
        "kind": kind,
        "title": title,
        "content": content,
    }))
}

/// The material an idempotent create is pinned to: content plus tags.
pub fn payload_material(kind: &str, title: &str, content: &Value, tags: &[String]) -> String {
    material(&json!({
        "kind": kind,
        "title": title,
        "content": content,
        "tags": tags,
    }))
}

/// Trim a title and admit it only when it is non-empty and within
/// [`TITLE_BYTES`]; `None` means the caller must refuse.
pub fn bounded_title(title: &str) -> Option<&str> {
    let title = title.trim();
    (!title.is_empty() && title.len() <= TITLE_BYTES).then_some(title)
}

/// Admit a `kind`'s closed content payload. Unknown kinds, fields, block
/// types and malformed or oversize content refuse.
pub fn validate_content(kind: &str, content: &Value) -> Result<(), Violation> {
    if !KINDS.contains(&kind) {
        return Err(invalid("invalid", "works kind must be doc, deck or sheet"));
    }
    let object = content
        .as_object()
        .ok_or_else(|| invalid("invalid", "works content must be an object"))?;
    let encoded = serde_json::to_vec(content).map_err(|error| Violation {
        kind: "internal",
        code: "internal",
        message: error.to_string(),
    })?;
    if encoded.len() > CONTENT_BYTES {
        return Err(invalid("invalid", "works content exceeds its bound"));
    }
    let allowed: &[&str] = match kind {
        "doc" => &["blocks"],
        "deck" => &["slides"],
        "sheet" => &["columns", "rows"],
        _ => unreachable!(),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid("invalid", "works content has unsupported fields"));
    }
    match kind {
        "doc" => {
            let blocks = content_array(object, "blocks")?;
            for block in blocks {
                let block = block
                    .as_object()
                    .ok_or_else(|| invalid("invalid", "works doc blocks must be objects"))?;
                if block
                    .keys()
                    .any(|key| !matches!(key.as_str(), "type" | "text" | "label"))
                {
                    return Err(invalid("invalid", "works doc block has unsupported fields"));
                }
                let kind = block
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("invalid", "works doc block type is required"))?;
                if !matches!(kind, "h2" | "p" | "callout") {
                    return Err(invalid("invalid", "works doc block type is unsupported"));
                }
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("invalid", "works doc block text is required"))?;
                if text.trim().is_empty() {
                    return Err(invalid("invalid", "works doc block text is required"));
                }
                if kind == "callout" {
                    if let Some(label) = block.get("label") {
                        if label.as_str().map(str::trim).is_none_or(str::is_empty) {
                            return Err(invalid("invalid", "works callout label must be a string"));
                        }
                    }
                }
            }
        }
        "deck" => {
            let slides = content_array(object, "slides")?;
            for slide in slides {
                let slide = slide
                    .as_object()
                    .ok_or_else(|| invalid("invalid", "works deck slides must be objects"))?;
                if slide
                    .keys()
                    .any(|key| !matches!(key.as_str(), "title" | "body" | "notes"))
                {
                    return Err(invalid(
                        "invalid",
                        "works deck slide has unsupported fields",
                    ));
                }
                for field in ["title", "body"] {
                    if slide
                        .get(field)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .is_none_or(str::is_empty)
                    {
                        return Err(invalid(
                            "invalid",
                            format!("works deck slide {field} is required"),
                        ));
                    }
                }
                if let Some(notes) = slide.get("notes") {
                    if notes.as_str().map(str::trim).is_none_or(str::is_empty) {
                        return Err(invalid("invalid", "works deck notes must be a string"));
                    }
                }
            }
        }
        "sheet" => {
            let columns = content_array(object, "columns")?;
            if columns.is_empty() {
                return Err(invalid("invalid", "works sheet columns are required"));
            }
            for column in columns {
                if column.as_str().map(str::trim).is_none_or(str::is_empty) {
                    return Err(invalid("invalid", "works sheet columns must be strings"));
                }
            }
            let rows = object
                .get("rows")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("invalid", "works sheet rows are required"))?;
            for row in rows {
                let cells = row
                    .as_array()
                    .ok_or_else(|| invalid("invalid", "works sheet rows must be arrays"))?;
                if cells.len() != columns.len() {
                    return Err(invalid("invalid", "works sheet row must match its columns"));
                }
                for cell in cells {
                    if !cell.is_string() && !cell.is_number() {
                        return Err(invalid(
                            "invalid",
                            "works sheet cells must be strings or numbers",
                        ));
                    }
                }
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn content_array<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Vec<Value>, Violation> {
    let values = object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("invalid", format!("works {key} are required")))?;
    if values.is_empty() {
        return Err(invalid("invalid", format!("works {key} are required")));
    }
    Ok(values)
}

/// The stored review/export columns of one item — the host's own proven
/// state, never a caller claim.
#[derive(Clone, Copy, Debug)]
pub struct State<'a> {
    /// The item's current revision.
    pub current: i64,
    /// The revision the last approval covered; `0` before any approval.
    pub reviewed: i64,
    /// `none` | `pending` | `approved` | `changes_requested`.
    pub review_status: &'a str,
    /// The revision a pending review was requested for.
    pub pending: Option<i64>,
    /// A trashed item is missing to every caller.
    pub trashed: bool,
}

/// `draft` | `edited` | `reviewed`, projected from stored state.
pub fn review_state(state: &State<'_>) -> &'static str {
    if state.review_status == "changes_requested" {
        "edited"
    } else if state.reviewed == 0 {
        "draft"
    } else if state.reviewed == state.current {
        "reviewed"
    } else {
        "edited"
    }
}

/// Whether the current revision is approved material a host may export.
pub fn export_state(state: &State<'_>) -> bool {
    !state.trashed && state.review_status == "approved" && state.reviewed == state.current
}

/// The UI's export explanation for any state.
pub fn export_reason(state: &State<'_>) -> &'static str {
    if state.review_status == "changes_requested" {
        "Export is blocked until the requested changes are reviewed."
    } else if state.reviewed == state.current && state.review_status == "approved" {
        "This revision is reviewed and ready for host export."
    } else {
        "Export is blocked until the current revision is reviewed."
    }
}

/// The review decision a caller chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Changes,
}

impl Decision {
    /// The status the host writes: an approval also records the revision as
    /// reviewed; requested changes leave the last approval in place.
    pub fn status(self) -> &'static str {
        match self {
            Self::Approve => "approved",
            Self::Changes => "changes_requested",
        }
    }
}

/// Admit the decision and note spelling before any state is read. The native
/// store checks these before loading the item, so this stays a separate step.
pub fn review_decision(decision: &str, note: Option<&str>) -> Result<Decision, Violation> {
    let decision = match decision {
        "approve" => Decision::Approve,
        "changes" => Decision::Changes,
        _ => {
            return Err(invalid(
                "invalid",
                "works decision must be approve or changes",
            ))
        }
    };
    if let Some(note) = note {
        if note.len() > NOTE_BYTES {
            return Err(invalid("invalid", "works review note exceeds its bound"));
        }
    }
    Ok(decision)
}

/// Admit a review of `revision`: the item must exist, the revision must be
/// current, and a review must be pending for exactly that revision.
pub fn review(state: &State<'_>, revision: i64) -> Result<(), Violation> {
    if state.trashed {
        return Err(invalid("not_found", "works content was not found"));
    }
    if state.current != revision {
        return Err(conflict("works content revision is stale"));
    }
    if state.review_status != "pending" || state.pending != Some(revision) {
        return Err(conflict("works review is not pending for this revision"));
    }
    Ok(())
}

/// A review needs two verified distinct humans: the stored author of the
/// revision and the presenting reviewer, neither the shared `operator`
/// principal.
pub fn reviewer(author: &str, reviewer: &str) -> Result<(), Violation> {
    if author == reviewer || author == "operator" || reviewer == "operator" {
        return Err(invalid(
            "forbidden",
            "a review requires two verified distinct humans",
        ));
    }
    Ok(())
}

/// Admit an export format: required, non-blank and bounded.
pub fn check_format(format: &str) -> Result<(), Violation> {
    if format.trim().is_empty() || format.len() > FORMAT_BYTES {
        return Err(invalid(
            "invalid",
            "works export format is required and bounded",
        ));
    }
    Ok(())
}

/// Admit an export of `revision`: only the current, approved revision is
/// exportable material.
pub fn export(state: &State<'_>, revision: i64) -> Result<(), Violation> {
    if state.trashed {
        return Err(invalid("not_found", "works content was not found"));
    }
    if state.current != revision {
        return Err(invalid(
            "needs_review",
            "only the current revision can be exported",
        ));
    }
    if state.review_status != "approved" || state.reviewed != revision {
        return Err(invalid(
            "needs_review",
            "works export requires an approved current revision",
        ));
    }
    Ok(())
}
