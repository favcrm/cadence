//! Versioned campaign email content, safe renderer and assistant proposals (CAD-782).
//!
//! A typed content document per installation/context/campaign lives in
//! the installation's record file: subject, preheader and a bounded
//! list of heading, paragraph and button blocks. The host validates
//! every byte — block counts/sizes, the single approved first-name
//! personalization token (with fallback), safe button URL
//! scheme/host, and subject/preheader controls — and refuses browser
//! HTML, script, unsafe paste and malformed data without mutation.
//!
//! The same exact revision renders deterministic sanitized
//! email-compatible HTML and a plain-text alternative; sender
//! identity, unsubscribe route and footer are host-owned constants
//! locked into every render and frozen payload, never editable
//! blocks. Revisions are CAS (expected-revision) with immutable
//! attribution; test-send and final-send preparation share the
//! content hash and renderer (actual SMTP submission is CAD-785/786).
//!
//! The left-chat assistant may propose subject/preheader/blocks from
//! the verified installation/context/campaign. A proposal is bounded,
//! attributed, bound to its source revision, and inert until the
//! operator explicitly applies it (new revision, approval
//! invalidated) or discards it (no change). There is no
//! agent-origin edit/approve/send path: every RPC in
//! `daemon::app_content_rpc` requires the operator connection, so an
//! agent caller or detached child is refused before any file opens.

use super::app_records::{email_shape_valid, RecordStore};
use super::app_runs::material_digest;
use super::*;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Subject is required, preheader optional; both are plain text.
pub const SUBJECT_BYTES: usize = 150;
pub const PREHEADER_BYTES: usize = 200;
/// Bounded visual blocks per draft/proposal.
pub const BLOCKS_MAX: usize = 12;
pub const HEADING_BYTES: usize = 120;
pub const PARAGRAPH_BYTES: usize = 2000;
pub const BUTTON_LABEL_BYTES: usize = 60;
pub const BUTTON_URL_BYTES: usize = 500;
/// Personalization fallback and sample-name bound.
pub const FALLBACK_BYTES: usize = 40;
/// Total `{{first_name|Fallback}}` tokens allowed per document.
pub const TOKENS_MAX: usize = 10;
pub const SAMPLE_NAME_BYTES: usize = 40;
/// Campaign/proposal listing ceiling.
pub const CONTENT_LIST_MAX: usize = 100;

/// Host-owned sender identity and unsubscribe/footer material. These
/// are locked into every render and frozen payload and are never
/// editable blocks, never operator input, never agent input.
pub const SENDER_NAME: &str = "Cadence CRM";
pub const SENDER_ADDRESS: &str = "noreply@cadence.invalid";
pub const UNSUBSCRIBE_BASE: &str = "https://cadence.invalid/unsubscribe";
pub const FOOTER_NOTE: &str = "You received this because you subscribed via Cadence CRM.";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Block {
    Heading { text: String },
    Paragraph { text: String },
    Button { label: String, url: String },
}

impl Block {
    /// Parse one untrusted block. Unknown shapes, unknown fields and
    /// unknown block types are refused; refusals never echo content.
    pub fn parse(body: &Value) -> std::result::Result<Self, BlockRefused> {
        let fields = body.as_object().ok_or(BlockRefused)?;
        match fields.get("type").and_then(Value::as_str) {
            Some("heading") => {
                if fields.len() != 2 {
                    return Err(BlockRefused);
                }
                let text = fields
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(BlockRefused)?;
                let block = Self::Heading {
                    text: text.to_string(),
                };
                block.validate().map_err(|_| BlockRefused)?;
                Ok(block)
            }
            Some("paragraph") => {
                if fields.len() != 2 {
                    return Err(BlockRefused);
                }
                let text = fields
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(BlockRefused)?;
                let block = Self::Paragraph {
                    text: text.to_string(),
                };
                block.validate().map_err(|_| BlockRefused)?;
                Ok(block)
            }
            Some("button") => {
                if fields.len() != 3 {
                    return Err(BlockRefused);
                }
                let label = fields
                    .get("label")
                    .and_then(Value::as_str)
                    .ok_or(BlockRefused)?;
                let url = fields
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or(BlockRefused)?;
                let block = Self::Button {
                    label: label.to_string(),
                    url: url.to_string(),
                };
                block.validate().map_err(|_| BlockRefused)?;
                Ok(block)
            }
            _ => Err(BlockRefused),
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::Heading { text } => {
                reject_text(text, HEADING_BYTES, false)?;
            }
            Self::Paragraph { text } => {
                reject_text(text, PARAGRAPH_BYTES, true)?;
            }
            Self::Button { label, url } => {
                reject_text(label, BUTTON_LABEL_BYTES, false)?;
                reject_url(url)?;
            }
        }
        Ok(())
    }

    fn canonical(&self) -> Value {
        match self {
            Self::Heading { text } => json!({"type": "heading", "text": text}),
            Self::Paragraph { text } => json!({"type": "paragraph", "text": text}),
            Self::Button { label, url } => {
                json!({"type": "button", "label": label, "url": url})
            }
        }
    }
}

/// Refusal marker for untrusted content shapes. Like
/// `ProfileRefused`, it carries no content — error text stays generic.
#[derive(Debug)]
pub struct BlockRefused;

impl std::fmt::Display for BlockRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "email content exceeds its supported shape or bounds")
    }
}

impl From<BlockRefused> for Error {
    fn from(_: BlockRefused) -> Self {
        Error::rejected("email content exceeds its supported shape or bounds")
    }
}

/// Plain-text hygiene shared by subject, preheader, headings,
/// paragraphs, labels and fallbacks: no markup, no script, no
/// controls (paragraphs may use `\n`), no merge-token smuggling
/// outside the one approved token grammar.
fn reject_text(text: &str, bound: usize, allow_newline: bool) -> Result<()> {
    const REFUSED: &str = "email text exceeds its supported shape or bounds";
    if text.is_empty() || text.len() > bound {
        return Err(Error::rejected(REFUSED));
    }
    for ch in text.chars() {
        if ch == '\n' && allow_newline {
            continue;
        }
        if ch.is_control() {
            return Err(Error::rejected(REFUSED));
        }
    }
    // No HTML/script/paste: angle brackets, backticks and stray
    // braces are never legitimate in plain-text email fields.
    if text.contains(['<', '>', '`']) {
        return Err(Error::rejected(REFUSED));
    }
    if text.contains('{') || text.contains('}') {
        // Braces are allowed only inside well-formed approved tokens.
        validate_tokens(text, 0)?;
    }
    // Scriptable schemes and event-handler paste have no place in
    // text fields, even as substrings.
    let lower = text.to_lowercase();
    for marker in [
        "javascript:",
        "data:",
        "vbscript:",
        "onerror=",
        "onload=",
        "onclick=",
        "<script",
        "expression(",
    ] {
        if lower.contains(marker) {
            return Err(Error::rejected(REFUSED));
        }
    }
    Ok(())
}

/// The only approved personalization: `{{first_name|Fallback}}`.
/// Every `{{...}}` span in the text must parse exactly; anything else
/// — `{%`, `{#`, `${`, unknown merge fields, empty fallback,
/// over-long fallback — is refused. Returns the token count so the
/// caller can bound the document total.
fn validate_tokens(text: &str, already: usize) -> Result<usize> {
    const REFUSED: &str = "email personalization token is not the approved first-name token";
    let mut count = already;
    let mut rest = text;
    loop {
        let Some(open) = rest.find("{{") else {
            if rest.contains("}}") || rest.contains("{%") || rest.contains("{#") {
                return Err(Error::rejected(REFUSED));
            }
            return Ok(count);
        };
        let after_open = &rest[open + 2..];
        let Some(close_rel) = after_open.find("}}") else {
            return Err(Error::rejected(REFUSED));
        };
        let token = &after_open[..close_rel];
        let mut parts = token.split('|');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("first_name"), Some(fallback), None) => {
                if !fallback_valid(fallback) {
                    return Err(Error::rejected(REFUSED));
                }
            }
            _ => return Err(Error::rejected(REFUSED)),
        }
        count += 1;
        if count > TOKENS_MAX {
            return Err(Error::rejected(
                "email personalization tokens exceed their bound",
            ));
        }
        rest = &after_open[close_rel + 2..];
    }
}

fn fallback_valid(fallback: &str) -> bool {
    !fallback.is_empty()
        && fallback.len() <= FALLBACK_BYTES
        && fallback
            .chars()
            .all(|ch| ch.is_alphabetic() || matches!(ch, ' ' | '-' | '\'' | '’'))
        && !fallback.chars().any(char::is_control)
        && !fallback.contains(['<', '>', '{', '}', '`'])
}

fn sample_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= SAMPLE_NAME_BYTES
        && name
            .chars()
            .all(|ch| ch.is_alphabetic() || matches!(ch, ' ' | '-' | '\'' | '’'))
}

/// Button URLs are `https` only, with a dotted host, no userinfo, no
/// whitespace/controls, no markup. `http`, `javascript:`, `data:`
/// and protocol-relative URLs are refused outright.
fn reject_url(url: &str) -> Result<()> {
    const REFUSED: &str = "email button URL exceeds its supported shape or bounds";
    if url.is_empty() || url.len() > BUTTON_URL_BYTES {
        return Err(Error::rejected(REFUSED));
    }
    if url.chars().any(char::is_control) || url.chars().any(char::is_whitespace) {
        return Err(Error::rejected(REFUSED));
    }
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::rejected(REFUSED))?;
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..host_end];
    if host.is_empty() || !host.contains('.') || host.contains('@') || host.contains(':') {
        return Err(Error::rejected(REFUSED));
    }
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
    {
        return Err(Error::rejected(REFUSED));
    }
    if url.contains(['<', '>', '"', '\'', '`', '\\', '{', '}']) {
        return Err(Error::rejected(REFUSED));
    }
    let lower = url.to_lowercase();
    for marker in ["javascript:", "data:", "vbscript:"] {
        if lower.contains(marker) {
            return Err(Error::rejected(REFUSED));
        }
    }
    Ok(())
}

fn reject_subject(subject: &str) -> Result<()> {
    if subject.is_empty() || subject.len() > SUBJECT_BYTES || subject.trim() != subject {
        return Err(Error::rejected(
            "email subject exceeds its supported shape or bounds",
        ));
    }
    reject_text(subject, SUBJECT_BYTES, false)
}

fn reject_preheader(preheader: &str) -> Result<()> {
    if preheader.len() > PREHEADER_BYTES {
        return Err(Error::rejected(
            "email preheader exceeds its supported shape or bounds",
        ));
    }
    if preheader.is_empty() {
        return Ok(());
    }
    reject_text(preheader, PREHEADER_BYTES, false)
}

/// A validated draft: subject, preheader and blocks with the
/// document-wide token bound enforced.
#[derive(Clone, Debug)]
pub struct Draft {
    pub subject: String,
    pub preheader: String,
    pub blocks: Vec<Block>,
}

impl Draft {
    /// Parse untrusted subject/preheader/blocks. Refusals never echo
    /// content; nothing mutates on refusal.
    pub fn parse(subject: &str, preheader: &str, blocks: &[Value]) -> Result<Self> {
        reject_subject(subject)?;
        reject_preheader(preheader)?;
        if blocks.is_empty() || blocks.len() > BLOCKS_MAX {
            return Err(Error::rejected("email blocks exceed their bound"));
        }
        let mut parsed = Vec::with_capacity(blocks.len());
        let mut tokens = validate_tokens(subject, 0)?;
        tokens = validate_tokens(preheader, tokens)?;
        for item in blocks {
            let block = Block::parse(item).map_err(Error::from)?;
            match &block {
                Block::Heading { text } | Block::Paragraph { text } => {
                    tokens = validate_tokens(text, tokens)?;
                }
                Block::Button { label, .. } => {
                    tokens = validate_tokens(label, tokens)?;
                }
            }
            parsed.push(block);
        }
        Ok(Self {
            subject: subject.to_string(),
            preheader: preheader.to_string(),
            blocks: parsed,
        })
    }

    fn canonical_blocks(&self) -> Value {
        Value::Array(self.blocks.iter().map(Block::canonical).collect())
    }
}

fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Substitute the approved token with the sample first name, falling
/// back to the declared fallback. Validation guarantees every
/// `{{...}}` span is well-formed, so substitution is total.
fn personalize(text: &str, sample: Option<&str>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(open) = rest.find("{{") else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        // Validated upstream: the close always exists.
        let Some(close_rel) = after_open.find("}}") else {
            out.push_str(rest);
            return out;
        };
        let token = &after_open[..close_rel];
        let fallback = token.split('|').nth(1).unwrap_or("");
        out.push_str(sample.unwrap_or(fallback));
        rest = &after_open[close_rel + 2..];
    }
}

/// Deterministic email-compatible HTML from one exact revision.
/// Fixed table layout, inline styles, escaped text, host-owned
/// sender/unsubscribe/footer locked at the end.
fn render_html(draft: &Draft, sample: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>");
    out.push_str(&html_escape(&personalize(&draft.subject, sample)));
    out.push_str("</title></head><body style=\"margin:0;padding:0;background:#f5f5f5;\">");
    if !draft.preheader.is_empty() {
        out.push_str("<div style=\"display:none;max-height:0;overflow:hidden;opacity:0;\">");
        out.push_str(&html_escape(&personalize(&draft.preheader, sample)));
        out.push_str("</div>");
    }
    out.push_str("<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr><td align=\"center\"><table role=\"presentation\" width=\"600\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#ffffff;margin:24px auto;\"><tr><td style=\"padding:32px;font-family:Arial,sans-serif;color:#222222;\">");
    for block in &draft.blocks {
        match block {
            Block::Heading { text } => {
                out.push_str("<h1 style=\"margin:0 0 16px;font-size:24px;line-height:1.3;\">");
                out.push_str(&html_escape(&personalize(text, sample)));
                out.push_str("</h1>");
            }
            Block::Paragraph { text } => {
                for para in personalize(text, sample).split('\n') {
                    out.push_str("<p style=\"margin:0 0 12px;font-size:16px;line-height:1.5;\">");
                    out.push_str(&html_escape(para));
                    out.push_str("</p>");
                }
            }
            Block::Button { label, url } => {
                out.push_str("<p style=\"margin:16px 0;\"><a href=\"");
                out.push_str(&html_escape(url));
                out.push_str("\" style=\"display:inline-block;padding:12px 24px;background:#1a73e8;color:#ffffff;text-decoration:none;border-radius:4px;font-size:16px;\">");
                out.push_str(&html_escape(&personalize(label, sample)));
                out.push_str("</a></p>");
            }
        }
    }
    out.push_str("<hr style=\"border:none;border-top:1px solid #dddddd;margin:24px 0;\"><p style=\"margin:0 0 8px;font-size:12px;color:#666666;\">");
    out.push_str(&html_escape(SENDER_LINE));
    out.push_str("</p><p style=\"margin:0;font-size:12px;color:#666666;\"><a href=\"");
    out.push_str(&html_escape(UNSUBSCRIBE_PLACEHOLDER));
    out.push_str("\">Unsubscribe</a> &middot; ");
    out.push_str(&html_escape(FOOTER_NOTE));
    out.push_str("</p></td></tr></table></td></tr></table></body></html>");
    out
}

const SENDER_LINE: &str = "Sent by Cadence CRM <noreply@cadence.invalid>";
const UNSUBSCRIBE_PLACEHOLDER: &str = "https://cadence.invalid/unsubscribe?token=RECIPIENT";

/// Deterministic plain-text alternative from the same revision.
fn render_text(draft: &Draft, sample: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str(&personalize(&draft.subject, sample));
    out.push('\n');
    if !draft.preheader.is_empty() {
        out.push_str(&personalize(&draft.preheader, sample));
        out.push('\n');
    }
    out.push('\n');
    for block in &draft.blocks {
        match block {
            Block::Heading { text } => {
                out.push_str(&personalize(text, sample));
                out.push_str("\n\n");
            }
            Block::Paragraph { text } => {
                out.push_str(&personalize(text, sample));
                out.push_str("\n\n");
            }
            Block::Button { label, url } => {
                out.push_str(&personalize(label, sample));
                out.push_str(": ");
                out.push_str(url);
                out.push_str("\n\n");
            }
        }
    }
    out.push_str("---\n");
    out.push_str(SENDER_LINE);
    out.push('\n');
    out.push_str("Unsubscribe: ");
    out.push_str(UNSUBSCRIBE_PLACEHOLDER);
    out.push('\n');
    out.push_str(FOOTER_NOTE);
    out.push('\n');
    out
}

fn content_digest(
    install: &str,
    context: &str,
    campaign: &str,
    revision: i64,
    draft: &Draft,
) -> String {
    material_digest(&json!({
        "domain": "cadence-app-content-v1",
        "install_id": install,
        "context_id": context,
        "campaign_id": campaign,
        "revision": revision,
        "subject": draft.subject,
        "preheader": draft.preheader,
        "blocks": draft.canonical_blocks(),
    }))
}

fn proposal_digest(
    install: &str,
    context: &str,
    campaign: &str,
    proposal: &str,
    draft: &Draft,
) -> String {
    material_digest(&json!({
        "domain": "cadence-app-content-proposal-v1",
        "install_id": install,
        "context_id": context,
        "campaign_id": campaign,
        "proposal_id": proposal,
        "subject": draft.subject,
        "preheader": draft.preheader,
        "blocks": draft.canonical_blocks(),
    }))
}

struct ContentRow {
    revision: i64,
    subject: String,
    preheader: String,
    blocks: Value,
    digest: String,
    approval_revision: Option<i64>,
    approval_digest: Option<String>,
}

struct ProposalRow {
    campaign: String,
    source_revision: i64,
    subject: String,
    preheader: String,
    blocks: String,
    digest: String,
    state: String,
    created: f64,
    decided: Option<f64>,
}

impl RecordStore {
    fn content_row(
        &self,
        conn: &Connection,
        context: &str,
        campaign: &str,
    ) -> Result<Option<ContentRow>> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(campaign, "campaign ID")?;
        conn.query_row(
            "SELECT revision,subject,preheader,blocks,content_digest,approval_revision,approval_digest FROM app_content_docs WHERE context_id=? AND campaign_id=?",
            params![context, campaign],
            |r| {
                let blocks: String = r.get(3)?;
                let parsed: Value = serde_json::from_str(&blocks).unwrap_or(Value::Null);
                Ok(ContentRow {
                    revision: r.get(0)?,
                    subject: r.get(1)?,
                    preheader: r.get(2)?,
                    blocks: parsed,
                    digest: r.get(4)?,
                    approval_revision: r.get(5)?,
                    approval_digest: r.get(6)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::internal(e.to_string()))
    }

    fn content_doc_json(&self, context: &str, campaign: &str, row: ContentRow) -> Value {
        let valid = row.approval_revision == Some(row.revision)
            && row.approval_digest.as_deref() == Some(row.digest.as_str());
        json!({
            "campaign_id": campaign,
            "install_id": self.install(),
            "context_id": context,
            "revision": row.revision,
            "subject": row.subject,
            "preheader": row.preheader,
            "blocks": row.blocks,
            "content_digest": row.digest,
            "approval": {
                "revision": row.approval_revision,
                "digest": row.approval_digest,
                "valid": valid,
            },
        })
    }

    /// Save a new operator revision with CAS. A missing doc requires
    /// no expected revision; an existing doc requires the observed
    /// one. Any save invalidates an earlier approval. Attribution is
    /// the fixed operator identity — the daemon never forwards a
    /// caller-chosen actor.
    pub fn app_content_save(
        &self,
        context: &str,
        campaign: &str,
        expected_revision: Option<i64>,
        draft: &Draft,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(campaign, "campaign ID")?;
        let blocks_text = serde_json::to_string(&draft.canonical_blocks())
            .map_err(|e| Error::internal(e.to_string()))?;
        let conn = self.conn();
        // IMMEDIATE: a racing saver blocks on the write lock first,
        // so exactly one revision wins and the loser is stale.
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| Error::internal(e.to_string()))?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT revision FROM app_content_docs WHERE context_id=? AND campaign_id=?",
                params![context, campaign],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?;
        let revision = match (current, expected_revision) {
            (None, None) => 1,
            (None, Some(_)) => {
                return Err(Error::rejected(
                    "email content is unknown; save without an expected revision",
                ));
            }
            (Some(_), None) => {
                return Err(Error::rejected(
                    "email content already exists; name the observed revision",
                ));
            }
            (Some(current), Some(expected)) => {
                if current != expected {
                    return Err(Error::rejected("email content revision is stale"));
                }
                current
                    .checked_add(1)
                    .ok_or_else(|| Error::rejected("email content revision exhausted"))?
            }
        };
        let digest = content_digest(self.install(), context, campaign, revision, draft);
        if current.is_none() {
            tx.execute(
                "INSERT INTO app_content_docs(context_id,campaign_id,revision,subject,preheader,blocks,content_digest,approval_revision,approval_digest,actor,created,updated) VALUES(?,?,?,?,?,?,?,NULL,NULL,'operator',?,?)",
                params![context, campaign, revision, draft.subject, draft.preheader, blocks_text, digest, now(), now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        } else {
            let changed = tx
                .execute(
                    "UPDATE app_content_docs SET revision=?,subject=?,preheader=?,blocks=?,content_digest=?,approval_revision=NULL,approval_digest=NULL,actor='operator',updated=? WHERE context_id=? AND campaign_id=? AND revision=?",
                    params![revision, draft.subject, draft.preheader, blocks_text, digest, now(), context, campaign, current],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if changed != 1 {
                return Err(Error::rejected("email content revision is stale"));
            }
        }
        tx.execute(
            "INSERT INTO app_content_revisions(context_id,campaign_id,revision,subject,preheader,blocks,content_digest,actor,origin,proposal_id,at) VALUES(?,?,?,?,?,?,?,'operator','operator',NULL,?)",
            params![context, campaign, revision, draft.subject, draft.preheader, blocks_text, digest, now()],
        )
        .map_err(|e| Error::internal(e.to_string()))?;
        tx.commit().map_err(|e| Error::internal(e.to_string()))?;
        drop(conn);
        Ok(json!({"content": self.app_content_show(context, campaign)?["content"]}))
    }

    pub fn app_content_show(&self, context: &str, campaign: &str) -> Result<Value> {
        let conn = self.conn();
        let row = self.content_row(&conn, context, campaign)?.ok_or_else(|| {
            Error::rejected("email content is unavailable for this installation and context")
        })?;
        Ok(json!({"content": self.content_doc_json(context, campaign, row)}))
    }

    pub fn app_content_list(&self, context: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT campaign_id FROM app_content_docs WHERE context_id=? ORDER BY campaign_id LIMIT ?")
            .map_err(|e| Error::internal(e.to_string()))?;
        let found = stmt
            .query_map(params![context, CONTENT_LIST_MAX as i64 + 1], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut ids = Vec::new();
        for row in found {
            ids.push(row.map_err(|e| Error::internal(e.to_string()))?);
            if ids.len() > CONTENT_LIST_MAX {
                return Err(Error::internal("email content listing exceeds its bound"));
            }
        }
        drop(stmt);
        let mut docs = Vec::with_capacity(ids.len());
        for id in &ids {
            let row = self.content_row(&conn, context, id)?.ok_or_else(|| {
                Error::internal("email content listing diverged from stored rows")
            })?;
            docs.push(self.content_doc_json(context, id, row));
        }
        Ok(json!({"contents": docs}))
    }

    fn draft_at(
        &self,
        conn: &Connection,
        context: &str,
        campaign: &str,
        revision: Option<i64>,
    ) -> Result<(i64, Draft, String)> {
        let row = self.content_row(conn, context, campaign)?.ok_or_else(|| {
            Error::rejected("email content is unavailable for this installation and context")
        })?;
        if let Some(wanted) = revision {
            if wanted != row.revision {
                // Immutable history: fetch the exact requested
                // revision; anything else is stale, not approximate.
                let (subject, preheader, blocks, digest): (String, String, String, String) = conn
                    .query_row(
                        "SELECT subject,preheader,blocks,content_digest FROM app_content_revisions WHERE context_id=? AND campaign_id=? AND revision=?",
                        params![context, campaign, wanted],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()
                    .map_err(|e| Error::internal(e.to_string()))?
                    .ok_or_else(|| Error::rejected("email content revision is unknown"))?;
                let raw: Value = serde_json::from_str(&blocks)
                    .map_err(|_| Error::rejected("email content revision is corrupt"))?;
                let items = raw
                    .as_array()
                    .ok_or_else(|| Error::rejected("email content revision is corrupt"))?;
                let mut parsed = Vec::with_capacity(items.len());
                for item in items {
                    parsed.push(
                        Block::parse(item)
                            .map_err(Error::from)
                            .map_err(|_: Error| {
                                Error::rejected("email content revision is corrupt")
                            })?,
                    );
                }
                let draft = Draft {
                    subject,
                    preheader,
                    blocks: parsed,
                };
                return Ok((wanted, draft, digest));
            }
        }
        let raw_blocks = row
            .blocks
            .as_array()
            .ok_or_else(|| Error::rejected("email content is corrupt"))?;
        let mut parsed = Vec::with_capacity(raw_blocks.len());
        for item in raw_blocks {
            parsed.push(
                Block::parse(item)
                    .map_err(Error::from)
                    .map_err(|_: Error| Error::rejected("email content is corrupt"))?,
            );
        }
        Ok((
            row.revision,
            Draft {
                subject: row.subject,
                preheader: row.preheader,
                blocks: parsed,
            },
            row.digest,
        ))
    }

    /// Render deterministic sanitized HTML and plain text from one
    /// exact revision (current when unnamed), with sample
    /// personalization. Sender/unsubscribe/footer are host-locked.
    pub fn app_content_render(
        &self,
        context: &str,
        campaign: &str,
        revision: Option<i64>,
        sample_first_name: Option<&str>,
    ) -> Result<Value> {
        if let Some(name) = sample_first_name {
            if !sample_name_valid(name) {
                return Err(Error::rejected(
                    "email sample name exceeds its supported shape or bounds",
                ));
            }
        }
        if let Some(wanted) = revision {
            if wanted <= 0 {
                return Err(Error::rejected("email content revision must be positive"));
            }
        }
        let conn = self.conn();
        let (resolved, draft, digest) = self.draft_at(&conn, context, campaign, revision)?;
        let html = render_html(&draft, sample_first_name);
        let text = render_text(&draft, sample_first_name);
        let render_digest = material_digest(&json!({
            "domain": "cadence-app-content-render-v1",
            "content_digest": digest,
            "html": html,
            "text": text,
        }));
        Ok(json!({
            "render": {
                "campaign_id": campaign,
                "install_id": self.install(),
                "context_id": context,
                "revision": resolved,
                "content_digest": digest,
                "sample_first_name": sample_first_name,
                "sender": {"name": SENDER_NAME, "address": SENDER_ADDRESS},
                "unsubscribe_url": UNSUBSCRIBE_PLACEHOLDER,
                "html": html,
                "text": text,
                "render_digest": render_digest,
            },
        }))
    }

    /// Record a bounded attributed assistant proposal. Inert: the
    /// draft does not change, approval does not change, nothing
    /// sends. Bound to the observed source revision; a proposal ID
    /// replays only behind identical bytes.
    pub fn app_content_propose(
        &self,
        context: &str,
        campaign: &str,
        proposal_id: &str,
        draft: &Draft,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(campaign, "campaign ID")?;
        crate::proto::identifier(proposal_id, "proposal ID")?;
        let conn = self.conn();
        let source_revision = self
            .content_row(&conn, context, campaign)?
            .map(|row| row.revision)
            .unwrap_or(0);
        let digest = proposal_digest(self.install(), context, campaign, proposal_id, draft);
        let blocks_text = serde_json::to_string(&draft.canonical_blocks())
            .map_err(|e| Error::internal(e.to_string()))?;
        if let Some(stored) = conn
            .query_row(
                "SELECT campaign_id,source_revision,content_digest FROM app_content_proposals WHERE context_id=? AND proposal_id=?",
                params![context, proposal_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
        {
            if stored.0 == campaign && stored.1 == source_revision && stored.2 == digest {
                drop(conn);
                return self.app_content_proposal_show(context, proposal_id);
            }
            return Err(Error::rejected("email proposal ID is already used"));
        }
        conn.execute(
            "INSERT INTO app_content_proposals(context_id,proposal_id,campaign_id,source_revision,subject,preheader,blocks,content_digest,actor,state,created,decided) VALUES(?,?,?,?,?,?,?,?,'assistant','pending',?,NULL)",
            params![context, proposal_id, campaign, source_revision, draft.subject, draft.preheader, blocks_text, digest, now()],
        )
        .map_err(|e| Error::internal(e.to_string()))?;
        drop(conn);
        self.app_content_proposal_show(context, proposal_id)
    }

    fn proposal_row(
        &self,
        conn: &Connection,
        context: &str,
        proposal_id: &str,
    ) -> Result<ProposalRow> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(proposal_id, "proposal ID")?;
        conn.query_row(
            "SELECT campaign_id,source_revision,subject,preheader,blocks,content_digest,state,created,decided FROM app_content_proposals WHERE context_id=? AND proposal_id=?",
            params![context, proposal_id],
            |r| {
                Ok(ProposalRow {
                    campaign: r.get(0)?,
                    source_revision: r.get(1)?,
                    subject: r.get(2)?,
                    preheader: r.get(3)?,
                    blocks: r.get(4)?,
                    digest: r.get(5)?,
                    state: r.get(6)?,
                    created: r.get(7)?,
                    decided: r.get(8)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::internal(e.to_string()))?
        .ok_or_else(|| {
            Error::rejected("email proposal is unavailable for this installation and context")
        })
    }

    pub fn app_content_proposal_show(&self, context: &str, proposal_id: &str) -> Result<Value> {
        let conn = self.conn();
        let row = self.proposal_row(&conn, context, proposal_id)?;
        let raw: Value = serde_json::from_str(&row.blocks).unwrap_or(Value::Null);
        Ok(json!({"proposal": {
            "proposal_id": proposal_id,
            "install_id": self.install(),
            "context_id": context,
            "campaign_id": row.campaign,
            "source_revision": row.source_revision,
            "subject": row.subject,
            "preheader": row.preheader,
            "blocks": raw,
            "content_digest": row.digest,
            "actor": "assistant",
            "state": row.state,
            "created": row.created,
            "decided": row.decided,
        }}))
    }

    pub fn app_content_proposal_list(
        &self,
        context: &str,
        campaign: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        if let Some(id) = campaign {
            crate::proto::identifier(id, "campaign ID")?;
        }
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT proposal_id FROM app_content_proposals WHERE context_id=? AND (? IS NULL OR campaign_id=?) ORDER BY proposal_id LIMIT ?")
            .map_err(|e| Error::internal(e.to_string()))?;
        let found = stmt
            .query_map(
                params![context, campaign, campaign, CONTENT_LIST_MAX as i64 + 1],
                |r| r.get::<_, String>(0),
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut ids = Vec::new();
        for row in found {
            ids.push(row.map_err(|e| Error::internal(e.to_string()))?);
            if ids.len() > CONTENT_LIST_MAX {
                return Err(Error::internal("email proposal listing exceeds its bound"));
            }
        }
        drop(stmt);
        let mut proposals = Vec::with_capacity(ids.len());
        for id in &ids {
            proposals.push(self.app_content_proposal_show(context, id)?["proposal"].clone());
        }
        Ok(json!({"proposals": proposals}))
    }

    /// Explicit operator Apply: a pending proposal becomes a new
    /// attributed revision (CAS against the current draft), the
    /// proposal marks applied, and any earlier approval is
    /// invalidated. A proposal whose source revision drifted behind
    /// the current draft is refused as stale — the operator
    /// re-reviews instead of silently merging.
    pub fn app_content_proposal_apply(
        &self,
        context: &str,
        proposal_id: &str,
        expected_revision: Option<i64>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(proposal_id, "proposal ID")?;
        let conn = self.conn();
        let proposal = self.proposal_row(&conn, context, proposal_id)?;
        if proposal.state != "pending" {
            return Err(Error::rejected("email proposal is already decided"));
        }
        let raw: Value = serde_json::from_str(&proposal.blocks)
            .map_err(|_| Error::rejected("email proposal is corrupt"))?;
        let items = raw
            .as_array()
            .ok_or_else(|| Error::rejected("email proposal is corrupt"))?;
        let mut blocks = Vec::with_capacity(items.len());
        for item in items {
            blocks.push(
                Block::parse(item)
                    .map_err(Error::from)
                    .map_err(|_: Error| Error::rejected("email proposal is corrupt"))?,
            );
        }
        let draft = Draft {
            subject: proposal.subject.clone(),
            preheader: proposal.preheader.clone(),
            blocks,
        };
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| Error::internal(e.to_string()))?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT revision FROM app_content_docs WHERE context_id=? AND campaign_id=?",
                params![context, proposal.campaign],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?;
        match current {
            None if proposal.source_revision != 0 => {
                return Err(Error::rejected("email proposal source revision is stale"));
            }
            Some(current) if current != proposal.source_revision => {
                return Err(Error::rejected("email proposal source revision is stale"));
            }
            _ => {}
        }
        match (current, expected_revision) {
            (Some(current), Some(expected)) if current != expected => {
                return Err(Error::rejected("email content revision is stale"));
            }
            (None, Some(_)) => {
                return Err(Error::rejected(
                    "email content is unknown; apply without an expected revision",
                ));
            }
            _ => {}
        }
        let revision = current.unwrap_or(0) + 1;
        let content_digest = content_digest(
            self.install(),
            context,
            &proposal.campaign,
            revision,
            &draft,
        );
        let stored = serde_json::to_string(&draft.canonical_blocks())
            .map_err(|e| Error::internal(e.to_string()))?;
        if current.is_none() {
            tx.execute(
                "INSERT INTO app_content_docs(context_id,campaign_id,revision,subject,preheader,blocks,content_digest,approval_revision,approval_digest,actor,created,updated) VALUES(?,?,?,?,?,?,?,NULL,NULL,'operator',?,?)",
                params![context, proposal.campaign, revision, draft.subject, draft.preheader, stored, content_digest, now(), now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        } else {
            let changed = tx
                .execute(
                    "UPDATE app_content_docs SET revision=?,subject=?,preheader=?,blocks=?,content_digest=?,approval_revision=NULL,approval_digest=NULL,actor='operator',updated=? WHERE context_id=? AND campaign_id=? AND revision=?",
                    params![revision, draft.subject, draft.preheader, stored, content_digest, now(), context, proposal.campaign, current],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if changed != 1 {
                return Err(Error::rejected("email content revision is stale"));
            }
        }
        tx.execute(
            "INSERT INTO app_content_revisions(context_id,campaign_id,revision,subject,preheader,blocks,content_digest,actor,origin,proposal_id,at) VALUES(?,?,?,?,?,?,?,'operator','proposal',?,?)",
            params![context, proposal.campaign, revision, draft.subject, draft.preheader, stored, content_digest, proposal_id, now()],
        )
        .map_err(|e| Error::internal(e.to_string()))?;
        let decided = tx
            .execute(
                "UPDATE app_content_proposals SET state='applied',decided=? WHERE context_id=? AND proposal_id=? AND state='pending'",
                params![now(), context, proposal_id],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        if decided != 1 {
            return Err(Error::rejected("email proposal is already decided"));
        }
        tx.commit().map_err(|e| Error::internal(e.to_string()))?;
        drop(conn);
        Ok(json!({"content": self.app_content_show(context, &proposal.campaign)?["content"]}))
    }

    /// Explicit operator Discard: the proposal marks discarded and
    /// the draft does not change — verified by comparing the content
    /// digest before and after in tests.
    pub fn app_content_proposal_discard(&self, context: &str, proposal_id: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(proposal_id, "proposal ID")?;
        let conn = self.conn();
        let changed = conn
            .execute(
                "UPDATE app_content_proposals SET state='discarded',decided=? WHERE context_id=? AND proposal_id=? AND state='pending'",
                params![now(), context, proposal_id],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        if changed != 1 {
            // Unknown IDs and already-decided proposals share one
            // refusal: neither admits whether the ID exists.
            return Err(Error::rejected("email proposal cannot be discarded"));
        }
        drop(conn);
        self.app_content_proposal_show(context, proposal_id)
    }

    /// Operator approval pins the exact current revision and digest.
    /// Any later save or apply clears the pin, so a stale approval
    /// never reads as current.
    pub fn app_content_approve(
        &self,
        context: &str,
        campaign: &str,
        expected_revision: i64,
    ) -> Result<Value> {
        if expected_revision <= 0 {
            return Err(Error::rejected(
                "expected content revision must be positive",
            ));
        }
        let conn = self.conn();
        let row = self.content_row(&conn, context, campaign)?.ok_or_else(|| {
            Error::rejected("email content is unavailable for this installation and context")
        })?;
        if row.revision != expected_revision {
            return Err(Error::rejected("email content revision is stale"));
        }
        let changed = conn
            .execute(
                "UPDATE app_content_docs SET approval_revision=?,approval_digest=?,updated=? WHERE context_id=? AND campaign_id=? AND revision=?",
                params![row.revision, row.digest, now(), context, campaign, row.revision],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        if changed != 1 {
            return Err(Error::rejected("email content revision is stale"));
        }
        drop(conn);
        self.app_content_show(context, campaign)
    }

    fn send_payload(
        &self,
        conn: &Connection,
        context: &str,
        campaign: &str,
        kind: &str,
        to_email: Option<&str>,
        audience_freeze_id: Option<&str>,
    ) -> Result<Value> {
        let (revision, draft, digest) = self.draft_at(conn, context, campaign, None)?;
        let html = render_html(&draft, None);
        let text = render_text(&draft, None);
        let audience_digest = match audience_freeze_id {
            None => Value::Null,
            Some(freeze) => {
                crate::proto::identifier(freeze, "freeze ID")?;
                let row: Option<(String, String)> = conn
                    .query_row(
                        "SELECT member_ids,digest FROM app_audience_freezes WHERE context_id=? AND freeze_id=?",
                        params![context, freeze],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()
                    .map_err(|e| Error::internal(e.to_string()))?;
                let (_, frozen) = row.ok_or_else(|| {
                    Error::rejected(
                        "audience freeze is unavailable for this installation and context",
                    )
                })?;
                Value::String(frozen)
            }
        };
        // Test and final preparation share this exact shape and the
        // same renderer: equal revisions always carry equal hashes.
        let payload = json!({
            "kind": kind,
            "campaign_id": campaign,
            "install_id": self.install(),
            "context_id": context,
            "content_revision": revision,
            "content_digest": digest,
            "audience_freeze_id": audience_freeze_id,
            "audience_digest": audience_digest,
            "sender": {"name": SENDER_NAME, "address": SENDER_ADDRESS},
            "unsubscribe_url": UNSUBSCRIBE_PLACEHOLDER,
            "headers": {
                "List-Unsubscribe": format!("<{UNSUBSCRIBE_PLACEHOLDER}>"),
                "List-Unsubscribe-Post": "List-Unsubscribe=One-Click",
            },
            "to_email": to_email,
            "html": html,
            "text": text,
            "payload_digest": material_digest(&json!({
                "domain": "cadence-app-content-send-v1",
                "kind": kind,
                "install_id": self.install(),
                "context_id": context,
                "campaign_id": campaign,
                "content_digest": digest,
                "audience_digest": audience_digest,
                "to_email": to_email,
                "html": html,
                "text": text,
            })),
        });
        Ok(payload)
    }

    /// Test-send preparation for one operator address: frozen content
    /// hash, locked sender/unsubscribe, same renderer as final send.
    /// No SMTP submission happens here (CAD-785/786 own delivery).
    pub fn app_content_test_prepare(
        &self,
        context: &str,
        campaign: &str,
        to_email: &str,
    ) -> Result<Value> {
        // The shared shape check plus a markup/paste refusal: test
        // recipients are operator-typed, never pasted HTML.
        if !email_shape_valid(to_email)
            || to_email.contains(['<', '>', '(', ')', '[', ']', '\\', '"', '\'', ';', ',', '`'])
        {
            return Err(Error::rejected(
                "email test recipient exceeds its supported shape or bounds",
            ));
        }
        let conn = self.conn();
        Ok(
            json!({"test_send": self.send_payload(&conn, context, campaign, "test", Some(to_email), None)?}),
        )
    }

    /// Final-send preparation freezes the same content hash (plus the
    /// optional audience freeze digest). Still no SMTP submission.
    pub fn app_content_send_prepare(
        &self,
        context: &str,
        campaign: &str,
        audience_freeze_id: Option<&str>,
    ) -> Result<Value> {
        let conn = self.conn();
        Ok(
            json!({"send": self.send_payload(&conn, context, campaign, "final", None, audience_freeze_id)?}),
        )
    }
}

impl Store {
    /// Best-effort audit event for a content write that already
    /// committed inside its installation file. Advisory like
    /// `note_app_audience`: digests only, never content.
    pub fn note_app_content(&self, install: &str, context: &str, action: &str, digest: &str) {
        let guard = match self.write_conn() {
            Ok(guard) => guard,
            Err(error) => {
                eprintln!("content audit event skipped: {error}");
                return;
            }
        };
        if Self::event(
            &guard,
            Self::DAEMON_STREAM,
            "app_content_changed",
            json!({"install_id": install, "context_id": context, "action": action, "digest": digest, "actor": "operator"}),
        )
        .is_err()
        {
            eprintln!("content audit event skipped: event write refused");
        }
    }
}
