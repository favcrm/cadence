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
//! email-compatible HTML and a plain-text alternative. Sender
//! identity and unsubscribe material travel as typed sender bindings
//! bound into the payload digest — never editable blocks. Every
//! binding in this ticket is operator-typed preview-only material:
//! no host verification exists yet, so renders and test
//! preparations carry explicitly labelled preview-only sender bytes
//! and final-send preparation always refuses until CAD-785 supplies
//! a host-custodied verified connection/sender and CAD-786 supplies
//! unsubscribe authority with per-recipient tokens. Revisions are
//! CAS (expected-revision) with immutable attribution; content
//! revision stays independent of sender/audience material (actual
//! SMTP submission is CAD-785/786, which consume the binding seam).
//! Approval pins content only; a rotated binding changes the test
//! digest while content approval stands, and must be re-prepared.
//!
//! Drafts reach the proposal flow through operator submission and
//! are recorded `actor='operator'` with `origin='operator-direct'`:
//! the operator connection proves the operator was present, never
//! that an assistant produced the text. `assistant` attribution
//! arrives only through `app_content_assistant_propose` (CAD-813),
//! which the daemon gates on a live assigned chat turn carrying a
//! server-verified App binding; receipt-shaped fields
//! (`assistant_receipt`, `turn_id`, `nonce`) stay refused on the
//! operator path. A proposal is bounded, bound to its
//! source revision, and inert until the operator explicitly applies
//! it (new revision, approval invalidated) or discards it (no
//! change). There is no agent-origin edit/approve/send path: every
//! mutating RPC in `daemon::app_content_rpc` except the
//! turn-bound assistant propose requires the operator
//! connection, so an agent caller or detached child is refused
//! before any file opens.

use super::app_records::{email_shape_valid, RecordStore};
use super::app_runs::material_digest;
use super::StoreConn;
use super::*;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Subject is required, preheader optional; both are plain text.
pub const SUBJECT_BYTES: usize = 150;
pub const PREHEADER_BYTES: usize = 200;
/// CAD-1058: bound on the optional human campaign name.
pub const NAME_CHARS: usize = 80;
pub const NAME_BYTES: usize = 240;
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

/// Preview-only sender identity and unsubscribe/footer material. These
/// `.invalid` bytes are render placeholders so previews never look
/// send-ready: they are labelled `preview_only` everywhere they
/// appear, and final-send preparation refuses until CAD-785/786
/// supply host-verified material. Saved bindings are operator-typed
/// preview-only material too — operator text and domain syntax are
/// never verification — so every render and test preparation in this
/// ticket is labelled preview-only. Preview material is never editable
/// blocks, never agent input.
pub const SENDER_NAME: &str = "Cadence CRM";
pub const SENDER_ADDRESS: &str = "noreply@cadence.invalid";
pub const UNSUBSCRIBE_BASE: &str = "https://cadence.invalid/unsubscribe";
pub const FOOTER_NOTE: &str = "You received this because you subscribed via Cadence CRM.";
/// Reserved binding ID for the built-in preview binding. Saved
/// bindings can never take this ID; final-send preparation never
/// accepts it.
pub const PREVIEW_BINDING_ID: &str = "preview";
/// Sender binding field bounds.
pub const BINDING_NAME_BYTES: usize = 80;
pub const UNSUBSCRIBE_BASE_BYTES: usize = 500;
pub const CONNECTION_ID_BYTES: usize = 128;
/// Sender binding listing ceiling.
pub const BINDING_LIST_MAX: usize = 100;

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
    /// CAD-1056: host-sanitised operator HTML body. When set it
    /// replaces `blocks` (kept empty) at render time.
    pub html: Option<String>,
    /// CAD-1056: operator plain-text override; generated when absent.
    pub text: Option<String>,
    /// CAD-1058: optional human campaign name (operator save only).
    /// Plain text; never part of the digest or a revision.
    pub name: Option<String>,
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
            html: None,
            text: None,
            name: None,
        })
    }

    /// CAD-1056: an operator draft whose body is pasted HTML. The
    /// HTML is sanitised here, so a `Draft` never holds raw markup.
    pub fn parse_html(subject: &str, preheader: &str, html: &str) -> Result<Self> {
        reject_subject(subject)?;
        reject_preheader(preheader)?;
        let clean = super::app_content_html::sanitize_html(html)?;
        let mut tokens = validate_tokens(subject, 0)?;
        tokens = validate_tokens(preheader, tokens)?;
        validate_tokens(&clean, tokens)?;
        Ok(Self {
            subject: subject.to_string(),
            preheader: preheader.to_string(),
            blocks: Vec::new(),
            html: Some(clean),
            text: None,
            name: None,
        })
    }

    /// CAD-1056: attach an optional plain-text override. It is bound,
    /// control-free, token-checked and may not carry the host footer.
    pub fn with_text(mut self, text: Option<&str>) -> Result<Self> {
        let Some(text) = text else {
            return Ok(self);
        };
        const BAD: &str = "email plain text exceeds its supported shape or bounds";
        if text.trim().is_empty()
            || text.len() > super::app_content_html::TEXT_OVERRIDE_BYTES
            || text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
            || text
                .to_ascii_lowercase()
                .contains(&FOOTER_NOTE.to_ascii_lowercase())
        {
            return Err(Error::rejected(BAD));
        }
        validate_tokens(text, 0)?;
        self.text = Some(text.to_string());
        Ok(self)
    }

    /// CAD-1058: attach an optional human campaign name. Bounded plain
    /// text: trimmed, one line, no markup and no merge tokens.
    pub fn with_name(mut self, name: Option<&str>) -> Result<Self> {
        let Some(name) = name else {
            return Ok(self);
        };
        if name.trim() != name
            || name.chars().count() > NAME_CHARS
            || name.contains(['{', '}'])
            || reject_text(name, NAME_BYTES, false).is_err()
        {
            return Err(Error::rejected(
                "campaign name exceeds its supported shape or bounds",
            ));
        }
        self.name = Some(name.to_string());
        Ok(self)
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

/// Sender material a render or payload is built with: either the
/// built-in preview placeholders or one saved binding revision.
/// Every view in this ticket is preview-only: the per-recipient token
/// stays the literal `RECIPIENT` marker — a placeholder, never a real
/// token — and CAD-786 substitutes real per-recipient tokens at send
/// time inside its own digest, never by editing frozen bytes here.
#[derive(Clone, Debug)]
pub struct BindingView {
    pub sender_name: String,
    pub sender_address: String,
    pub unsubscribe_base: String,
    /// CAD-786: the exact per-recipient unsubscribe URL when the
    /// sender owns real token authority — `None` keeps the
    /// `?token=RECIPIENT` preview marker.
    pub unsubscribe_url: Option<String>,
}

fn preview_binding_view() -> BindingView {
    BindingView {
        sender_name: SENDER_NAME.to_string(),
        sender_address: SENDER_ADDRESS.to_string(),
        unsubscribe_base: UNSUBSCRIBE_BASE.to_string(),
        unsubscribe_url: None,
    }
}

fn sender_line(binding: &BindingView) -> String {
    format!(
        "Sent by {} <{}>",
        binding.sender_name, binding.sender_address
    )
}

fn unsubscribe_url(binding: &BindingView) -> String {
    binding
        .unsubscribe_url
        .clone()
        .unwrap_or_else(|| format!("{}?token=RECIPIENT", binding.unsubscribe_base))
}

/// Every binding in this ticket is preview-only: no host verification
/// exists yet, so domain syntax is never consulted. CAD-785 will
/// supply host-custodied connection identity plus verified-sender
/// evidence and CAD-786 unsubscribe authority; until then the only
/// honest value is `true`.
fn binding_preview_only() -> bool {
    true
}

fn unsubscribe_host(base: &str) -> Option<&str> {
    let rest = base.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Strict operator-typed email: the shared shape plus a
/// markup/paste refusal. Shared by test recipients and sender
/// addresses so neither can smuggle HTML.
fn strict_email(address: &str) -> bool {
    email_shape_valid(address)
        && !address.contains(['<', '>', '(', ')', '[', ']', '\\', '"', '\'', ';', ',', '`'])
}

fn reject_sender_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > BINDING_NAME_BYTES || name.trim() != name {
        return Err(Error::rejected(
            "email sender name exceeds its supported shape or bounds",
        ));
    }
    reject_text(name, BINDING_NAME_BYTES, false)
}

fn reject_unsubscribe_base(base: &str) -> Result<()> {
    const REFUSED: &str = "email unsubscribe base exceeds its supported shape or bounds";
    if base.is_empty() || base.len() > UNSUBSCRIBE_BASE_BYTES {
        return Err(Error::rejected(REFUSED));
    }
    if base.chars().any(char::is_control) || base.chars().any(char::is_whitespace) {
        return Err(Error::rejected(REFUSED));
    }
    let rest = base
        .strip_prefix("https://")
        .ok_or_else(|| Error::rejected(REFUSED))?;
    let host = unsubscribe_host(base).ok_or_else(|| Error::rejected(REFUSED))?;
    if host.is_empty() || !host.contains('.') || host.contains('@') || host.contains(':') {
        return Err(Error::rejected(REFUSED));
    }
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
    {
        return Err(Error::rejected(REFUSED));
    }
    let _ = rest;
    if base.contains(['<', '>', '"', '\'', '`', '\\', '{', '}']) {
        return Err(Error::rejected(REFUSED));
    }
    Ok(())
}

fn reject_connection_id(connection: &str) -> Result<()> {
    // Opaque operator note naming the intended CAD-785 connection.
    // This string is never authority: no connection is resolved or
    // verified here, so a present, absent or fictitious value changes
    // nothing about send readiness. Bounded opaque text until CAD-785
    // types the connection. Never markup, never blank.
    if connection.is_empty()
        || connection.len() > CONNECTION_ID_BYTES
        || connection.chars().any(char::is_control)
        || connection.chars().any(char::is_whitespace)
        || connection.contains(['<', '>', '"', '\'', '`', '\\', '{', '}'])
    {
        return Err(Error::rejected(
            "email sender connection exceeds its supported shape or bounds",
        ));
    }
    Ok(())
}

/// Deterministic email-compatible HTML from one exact revision.
/// Fixed table layout, inline styles, escaped text, sender/unsubscribe
/// material from the given binding locked at the end.
fn render_html(draft: &Draft, sample: Option<&str>, binding: &BindingView) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>");
    out.push_str(&html_escape(&personalize(&draft.subject, sample)));
    out.push_str("</title></head><body style=\"margin:0;padding:0;background:#f5f5f5;\">");
    if !draft.preheader.is_empty() {
        out.push_str("<div style=\"display:none;max-height:0;overflow:hidden;opacity:0;\">");
        out.push_str(&html_escape(&personalize(&draft.preheader, sample)));
        out.push_str("</div>");
    }
    // CAD-1014: the content column must shrink to the preview iframe —
    // a fixed `width="600"` clips under the narrow-390 CRM preview. Keep
    // the 600px desktop measure but cap it as a style (never a fixed
    // attribute) so the column fills the frame at narrower widths.
    out.push_str("<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr><td align=\"center\"><table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#ffffff;max-width:600px;margin:24px auto;\"><tr><td style=\"padding:32px;font-family:Arial,sans-serif;color:#222222;\">");
    if let Some(html) = &draft.html {
        // Sanitised at save time; the sample name and fallbacks are
        // alphabetic by grammar, so substitution cannot add markup.
        out.push_str(&personalize(html, sample));
    }
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
    out.push_str(&html_escape(&sender_line(binding)));
    out.push_str("</p><p style=\"margin:0;font-size:12px;color:#666666;\"><a href=\"");
    out.push_str(&html_escape(&unsubscribe_url(binding)));
    out.push_str("\">Unsubscribe</a> &middot; ");
    out.push_str(&html_escape(FOOTER_NOTE));
    out.push_str("</p></td></tr></table></td></tr></table></body></html>");
    out
}

/// Deterministic plain-text alternative from the same revision.
fn render_text(draft: &Draft, sample: Option<&str>, binding: &BindingView) -> String {
    let mut out = String::new();
    out.push_str(&personalize(&draft.subject, sample));
    out.push('\n');
    if !draft.preheader.is_empty() {
        out.push_str(&personalize(&draft.preheader, sample));
        out.push('\n');
    }
    out.push('\n');
    if let Some(text) = &draft.text {
        out.push_str(&personalize(text, sample));
        out.push_str("\n\n");
    } else if let Some(html) = &draft.html {
        out.push_str(&super::app_content_html::html_to_text(&personalize(
            html, sample,
        )));
        out.push_str("\n\n");
    }
    for block in &draft.blocks {
        if draft.text.is_some() {
            break;
        }
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
    out.push_str(&sender_line(binding));
    out.push('\n');
    out.push_str("Unsubscribe: ");
    out.push_str(&unsubscribe_url(binding));
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
    let mut doc = json!({
        "domain": "cadence-app-content-v1",
        "install_id": install,
        "context_id": context,
        "campaign_id": campaign,
        "revision": revision,
        "subject": draft.subject,
        "preheader": draft.preheader,
        "blocks": draft.canonical_blocks(),
    });
    // CAD-1056: present only when set, so every earlier digest is
    // unchanged and a body or text change always changes the digest.
    if let Some(html) = &draft.html {
        doc["html"] = json!(html);
    }
    if let Some(text) = &draft.text {
        doc["text_override"] = json!(text);
    }
    material_digest(&doc)
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

/// Daemon-resolved provenance for a verified assistant proposal
/// (CAD-813): the assigned agent, the chat message and the
/// operator-minted, host-stamped proposal request the daemon proved
/// against its own turn rows, server-verified App binding and live
/// installation/context. Never caller authority — the RPC layer
/// derives every field; campaign and source revision come from the
/// stamped request, never agent text.
pub struct AssistantClaim<'a> {
    pub agent: &'a str,
    pub message: &'a str,
    pub request: &'a str,
}

struct ContentRow {
    name: Option<String>,
    draft_segment_id: Option<String>,
    revision: i64,
    subject: String,
    preheader: String,
    blocks: Value,
    digest: String,
    approval_revision: Option<i64>,
    approval_digest: Option<String>,
    html: Option<String>,
    text: Option<String>,
}

struct ProposalRow {
    campaign: String,
    source_revision: i64,
    subject: String,
    preheader: String,
    blocks: String,
    digest: String,
    actor: String,
    origin: String,
    receipt_message: Option<String>,
    receipt_agent: Option<String>,
    receipt_request: Option<String>,
    state: String,
    created: f64,
    decided: Option<f64>,
}

/// One operator-minted proposal request row (CAD-813): the
/// host-stamped campaign and content source revision a chat message
/// may redeem exactly once.
struct ProposalRequestRow {
    campaign: String,
    source_revision: i64,
    message: String,
    state: String,
    used_by: Option<String>,
    created: f64,
    decided: Option<f64>,
}

/// A SQLite constraint violation is a lost claim race, never a
/// crash: the proposal id, the per-message claim or the request
/// spent under a concurrent writer. Callers map it to the bounded
/// already-used/already-claimed refusal.
fn is_claim_conflict(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == rusqlite::ffi::ErrorCode::ConstraintViolation
    )
}

/// Validated sender material for a binding save. The store
/// validates every field; the RPC layer only parses transport types.
pub struct BindingDraft<'a> {
    pub sender_name: &'a str,
    pub sender_address: &'a str,
    pub unsubscribe_base: &'a str,
    pub connection_id: Option<&'a str>,
}

/// Validated clone selection inputs; authority remains in the URL scope.
#[derive(Clone, Copy)]
pub struct CloneOptions<'a> {
    pub expected_revision: i64,
    pub name: &'a str,
    pub copy_audience: bool,
    pub source_freeze_id: Option<&'a str>,
    pub copy_sender: bool,
    pub source_binding_id: Option<&'a str>,
}

/// One saved binding row.
struct BindingRecord {
    binding_id: String,
    revision: i64,
    sender_name: String,
    sender_address: String,
    unsubscribe_base: String,
    connection_id: Option<String>,
    digest: String,
}

/// Send-preparation scope: content selection plus frozen attachments.
struct SendPrep<'a> {
    context: &'a str,
    campaign: &'a str,
    kind: &'a str,
    to_email: Option<&'a str>,
    audience_freeze_id: Option<&'a str>,
    binding: &'a ResolvedBinding,
}

fn binding_digest(
    install: &str,
    context: &str,
    binding: &str,
    revision: i64,
    draft: &BindingDraft,
) -> String {
    material_digest(&json!({
        "domain": "cadence-app-sender-binding-v1",
        "install_id": install,
        "context_id": context,
        "binding_id": binding,
        "revision": revision,
        "sender_name": draft.sender_name,
        "sender_address": draft.sender_address,
        "unsubscribe_base": draft.unsubscribe_base,
        "connection_id": draft.connection_id,
    }))
}

/// A resolved sender binding: saved revision material plus its
/// derived preview state. The built-in preview binding resolves
/// without a row; every other ID must name a saved row in this
/// installation and context.
struct ResolvedBinding {
    binding_id: String,
    revision: i64,
    digest: String,
    connection_id: Option<String>,
    preview_only: bool,
    view: BindingView,
}

/// A stored assistant-draft proposal row read back for idempotent
/// replay — campaign, source revision, content digest and the
/// message/agent receipt it was claimed under (CAD-1014).
struct PriorDraft {
    campaign_id: String,
    source_revision: i64,
    content_digest: String,
    receipt_message: Option<String>,
    receipt_agent: Option<String>,
}

impl RecordStore {
    fn content_row(
        &self,
        conn: &impl super::StoreConn,
        context: &str,
        campaign: &str,
    ) -> Result<Option<ContentRow>> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(campaign, "campaign ID")?;
        conn.query_row(
            "SELECT revision,subject,preheader,blocks,content_digest,approval_revision,approval_digest,html,text_override,name,draft_segment_id FROM app_content_docs WHERE context_id=? AND campaign_id=?",
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
                    html: r.get(7)?,
                    text: r.get(8)?,
                    name: r.get(9)?,
                    draft_segment_id: r.get(10)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::internal(e.to_string()))
    }

    fn content_doc_json(&self, context: &str, campaign: &str, row: ContentRow) -> Value {
        let valid = row.approval_revision == Some(row.revision)
            && row.approval_digest.as_deref() == Some(row.digest.as_str());
        let draft_audience = row.draft_segment_id.as_ref().map_or(
            Value::Null,
            |segment_id| json!({"mode":"segment","segment_id":segment_id}),
        );
        json!({
            "campaign_id": campaign,
            "name": row.name,
            "draft_audience": draft_audience,
            "install_id": self.install(),
            "context_id": context,
            "revision": row.revision,
            "subject": row.subject,
            "preheader": row.preheader,
            "blocks": row.blocks,
            "mode": if row.html.is_some() { "html" } else { "blocks" },
            "html": row.html,
            "text_override": row.text,
            "content_digest": row.digest,
            "approval": {
                "revision": row.approval_revision,
                "digest": row.approval_digest,
                "valid": valid,
                "scope": "content-only",
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
        self.write_tx(|tx| {
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
                    "INSERT INTO app_content_docs(context_id,campaign_id,revision,subject,preheader,blocks,content_digest,approval_revision,approval_digest,actor,created,updated,html,text_override,name) VALUES(?,?,?,?,?,?,?,NULL,NULL,'operator',?,?,?,?,?)",
                    params![context, campaign, revision, draft.subject, draft.preheader, blocks_text, digest, now(), now(), draft.html, draft.text, draft.name],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            } else {
                let changed = tx
                    .execute(
                        "UPDATE app_content_docs SET revision=?,subject=?,preheader=?,blocks=?,content_digest=?,approval_revision=NULL,approval_digest=NULL,actor='operator',updated=?,html=?,text_override=?,name=COALESCE(?,name) WHERE context_id=? AND campaign_id=? AND revision=?",
                        params![revision, draft.subject, draft.preheader, blocks_text, digest, now(), draft.html, draft.text, draft.name, context, campaign, current],
                    )
                    .map_err(|e| Error::internal(e.to_string()))?;
                if changed != 1 {
                    return Err(Error::rejected("email content revision is stale"));
                }
            }
            tx.execute(
                "INSERT INTO app_content_revisions(context_id,campaign_id,revision,subject,preheader,blocks,content_digest,actor,origin,proposal_id,at,html,text_override) VALUES(?,?,?,?,?,?,?,'operator','operator',NULL,?,?,?)",
                params![context, campaign, revision, draft.subject, draft.preheader, blocks_text, digest, now(), draft.html, draft.text],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            Ok(())
        })?;
        Ok(json!({"content": self.app_content_show(context, campaign)?["content"]}))
    }

    /// Create an assistant-attributed, unsent campaign draft. The
    /// campaign must be new; the write stores the connection-derived
    /// agent attribution and never creates an approval or send receipt.
    pub fn app_content_create_assistant(
        &self,
        context: &str,
        campaign: &str,
        actor: &str,
        operation_id: &str,
        draft: &Draft,
        segment_id: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(campaign, "campaign ID")?;
        crate::proto::identifier(actor, "assistant identity")?;
        crate::proto::identifier(operation_id, "operation ID")?;
        if let Some(segment_id) = segment_id {
            crate::proto::identifier(segment_id, "segment ID")?;
        }
        let blocks_text = serde_json::to_string(&draft.canonical_blocks())
            .map_err(|e| Error::internal(e.to_string()))?;
        self.write_tx(|tx| {
            let existing: Option<String> = tx.query_row(
                "SELECT content_digest FROM app_content_docs WHERE context_id=? AND campaign_id=?",
                params![context,campaign], |row| row.get(0),
            ).optional().map_err(|e| Error::internal(e.to_string()))?;
            if existing.is_some() { return Err(Error::rejected("campaign already exists; assistant create is fresh-draft only")); }
            if let Some(segment_id) = segment_id {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM app_segments WHERE context_id=? AND id=?)",
                    params![context,segment_id], |row| row.get(0),
                ).map_err(|e| Error::internal(e.to_string()))?;
                if !exists { return Err(Error::rejected("segment is unavailable in this context")); }
            }
            let revision = 1;
            let digest = content_digest(self.install(), context, campaign, revision, draft);
            tx.execute(
                "INSERT INTO app_content_docs(context_id,campaign_id,revision,subject,preheader,blocks,content_digest,approval_revision,approval_digest,actor,created,updated,html,text_override,name,draft_segment_id) VALUES(?,?,?,?,?,?,?,NULL,NULL,?,?,?,?,?,?,?)",
                params![context,campaign,revision,draft.subject,draft.preheader,blocks_text,digest,actor,now(),now(),draft.html,draft.text,draft.name,segment_id],
            ).map_err(|e| Error::internal(e.to_string()))?;
            tx.execute(
                "INSERT INTO app_content_revisions(context_id,campaign_id,revision,subject,preheader,blocks,content_digest,actor,origin,proposal_id,at,html,text_override) VALUES(?,?,?,?,?,?,?,?,'proposal',?,?,?,?)",
                params![context,campaign,revision,draft.subject,draft.preheader,blocks_text,digest,actor,operation_id,now(),draft.html,draft.text],
            ).map_err(|e| Error::internal(e.to_string()))?;
            Ok(())
        })?;
        Ok(json!({"content":self.app_content_show(context,campaign)?["content"]}))
    }

    /// Clone saved content into a host-minted, initial-revision draft.
    /// Selection references are explicitly supplied by the operator UI
    /// because the content store has no canonical campaign-selection link;
    /// they are re-resolved in this context and never copy frozen recipients.
    pub fn app_content_clone(
        &self,
        context: &str,
        source_campaign: &str,
        options: &CloneOptions<'_>,
    ) -> Result<Value> {
        let CloneOptions {
            expected_revision,
            name,
            copy_audience,
            source_freeze_id,
            copy_sender,
            source_binding_id,
        } = *options;
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(source_campaign, "campaign ID")?;
        if expected_revision <= 0 {
            return Err(Error::rejected(
                "expected content revision must be positive",
            ));
        }
        if copy_audience != source_freeze_id.is_some() || copy_sender != source_binding_id.is_some()
        {
            return Err(Error::rejected("campaign clone selections are malformed"));
        }
        if name.trim() != name
            || name.chars().count() > NAME_CHARS
            || name.contains(['{', '}'])
            || reject_text(name, NAME_BYTES, false).is_err()
        {
            return Err(Error::rejected(
                "campaign name exceeds its supported shape or bounds",
            ));
        }
        if let Some(freeze) = source_freeze_id {
            crate::proto::identifier(freeze, "freeze ID")?;
        }
        if let Some(binding) = source_binding_id {
            crate::proto::identifier(binding, "sender binding ID")?;
            if binding == PREVIEW_BINDING_ID {
                return Err(Error::rejected("sender binding is unavailable for cloning"));
            }
        }

        let (target, audience, sender_binding_id) = self.write_tx(|tx| {
            let source = self.content_row(tx, context, source_campaign)?.ok_or_else(|| {
                Error::rejected("email content is unavailable for this installation and context")
            })?;
            if source.revision != expected_revision {
                return Err(Error::rejected("email content revision is stale"));
            }
            let audience = match source_freeze_id {
                None => None,
                Some(freeze) => {
                    let found: Option<(String, Option<String>)> = tx
                        .query_row(
                            "SELECT base,exclusion_list_id FROM app_audience_freezes WHERE context_id=? AND freeze_id=?",
                            params![context, freeze],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()
                        .map_err(|e| Error::internal(e.to_string()))?;
                    let (base, exclusion_list_id) = found.ok_or_else(|| {
                        Error::rejected("audience selection is unavailable for this installation and context")
                    })?;
                    let base: Value = serde_json::from_str(&base)
                        .map_err(|_| Error::rejected("audience selection is corrupt"))?;
                    crate::store::app_audiences::AudienceBase::parse(&base)
                        .map_err(|_| Error::rejected("audience selection is corrupt"))?;
                    Some(json!({"base": base, "exclusion_list_id": exclusion_list_id}))
                }
            };
            if let Some(binding) = source_binding_id {
                let found: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM app_sender_bindings WHERE context_id=? AND binding_id=?)",
                        params![context, binding],
                        |row| row.get(0),
                    )
                    .map_err(|e| Error::internal(e.to_string()))?;
                if !found {
                    return Err(Error::rejected(
                        "sender binding is unavailable for this installation and context",
                    ));
                }
            }

            // The clone uses exactly the saved format and payload bytes;
            // source rows are host-sanitized and the original is untouched.
            let mut draft = if let Some(html) = source.html.as_deref() {
                Draft::parse_html(&source.subject, &source.preheader, html)?
            } else {
                let blocks = source.blocks.as_array().ok_or_else(|| {
                    Error::rejected("stored email content is unavailable")
                })?;
                Draft::parse(&source.subject, &source.preheader, blocks)?
            };
            if let Some(html) = source.html {
                draft.html = Some(html);
            }
            draft = draft.with_text(source.text.as_deref())?.with_name(Some(name))?;
            let target = format!("campaign-{}", uuid::Uuid::new_v4().simple());
            let blocks_text = serde_json::to_string(&draft.canonical_blocks())
                .map_err(|e| Error::internal(e.to_string()))?;
            let digest = content_digest(self.install(), context, &target, 1, &draft);
            tx.execute(
                "INSERT INTO app_content_docs(context_id,campaign_id,revision,subject,preheader,blocks,content_digest,approval_revision,approval_digest,actor,created,updated,html,text_override,name) VALUES(?,?,1,?,?,?,?,NULL,NULL,'operator',?,?,?, ?,?)",
                params![context, target, draft.subject, draft.preheader, blocks_text, digest, now(), now(), draft.html, draft.text, draft.name],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            tx.execute(
                "INSERT INTO app_content_revisions(context_id,campaign_id,revision,subject,preheader,blocks,content_digest,actor,origin,proposal_id,at,html,text_override) VALUES(?,?,1,?,?,?,?,'operator','operator',NULL,?,?,?)",
                params![context, target, draft.subject, draft.preheader, blocks_text, digest, now(), draft.html, draft.text],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            Ok((target, audience, source_binding_id.map(str::to_owned)))
        })?;
        let content = self.app_content_show(context, &target)?["content"].clone();
        Ok(json!({
            "content": content,
            "starter_selection": {
                "audience": audience,
                "sender_binding_id": sender_binding_id,
            }
        }))
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
        conn: &impl super::StoreConn,
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
                type Hist = (
                    String,
                    String,
                    String,
                    String,
                    Option<String>,
                    Option<String>,
                );
                let (subject, preheader, blocks, digest, html, text): Hist = conn
                    .query_row(
                        "SELECT subject,preheader,blocks,content_digest,html,text_override FROM app_content_revisions WHERE context_id=? AND campaign_id=? AND revision=?",
                        params![context, campaign, wanted],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
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
                    html,
                    text,
                    name: None,
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
                html: row.html,
                text: row.text,
                name: None,
            },
            row.digest,
        ))
    }

    /// Resolve a sender binding: the reserved preview ID resolves to
    /// the built-in placeholders without a row; any other ID must
    /// name a saved binding in this installation and context.
    fn resolve_binding(
        &self,
        conn: &impl super::StoreConn,
        context: &str,
        binding_id: Option<&str>,
    ) -> Result<ResolvedBinding> {
        let Some(id) = binding_id else {
            let preview = BindingDraft {
                sender_name: SENDER_NAME,
                sender_address: SENDER_ADDRESS,
                unsubscribe_base: UNSUBSCRIBE_BASE,
                connection_id: None,
            };
            return Ok(ResolvedBinding {
                binding_id: PREVIEW_BINDING_ID.to_string(),
                revision: 0,
                digest: binding_digest(self.install(), context, PREVIEW_BINDING_ID, 0, &preview),
                connection_id: None,
                preview_only: true,
                view: preview_binding_view(),
            });
        };
        let record = self.binding_record(conn, context, id)?;
        Ok(ResolvedBinding {
            binding_id: id.to_string(),
            revision: record.revision,
            digest: record.digest.clone(),
            connection_id: record.connection_id.clone(),
            preview_only: binding_preview_only(),
            view: BindingView {
                sender_name: record.sender_name.clone(),
                sender_address: record.sender_address.clone(),
                unsubscribe_base: record.unsubscribe_base.clone(),
                unsubscribe_url: None,
            },
        })
    }

    fn binding_record(
        &self,
        conn: &impl super::StoreConn,
        context: &str,
        binding_id: &str,
    ) -> Result<BindingRecord> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(binding_id, "sender binding ID")?;
        let (revision, name, address, base, connection, digest): (
            i64,
            String,
            String,
            String,
            Option<String>,
            String,
        ) = conn
            .query_row(
                "SELECT revision,sender_name,sender_address,unsubscribe_base,connection_id,binding_digest FROM app_sender_bindings WHERE context_id=? AND binding_id=?",
                params![context, binding_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
            .ok_or_else(|| {
                Error::rejected(
                    "email sender binding is unavailable for this installation and context",
                )
            })?;
        Ok(BindingRecord {
            binding_id: binding_id.to_string(),
            revision,
            sender_name: name,
            sender_address: address,
            unsubscribe_base: base,
            connection_id: connection,
            digest,
        })
    }

    fn binding_json(&self, context: &str, record: &BindingRecord) -> Value {
        json!({
            "binding_id": record.binding_id,
            "install_id": self.install(),
            "context_id": context,
            "revision": record.revision,
            "sender": {"name": record.sender_name, "address": record.sender_address},
            "unsubscribe_base": record.unsubscribe_base,
            "connection_id": record.connection_id,
            "preview_only": binding_preview_only(),
            "binding_digest": record.digest,
        })
    }

    /// Save operator-typed preview-only sender material with CAS. The
    /// saved rows preserve the typed binding/content digest boundary
    /// for CAD-785/786, but nothing saved here is verified: the
    /// operator connection proves the operator typed the bytes, never
    /// that an SMTP connection exists, that the sender is authorized,
    /// or that the unsubscribe base carries authority. Every saved
    /// binding resolves `preview_only: true` and final-send
    /// preparation refuses it. The reserved preview ID is
    /// unaddressable here.
    pub fn app_sender_binding_save(
        &self,
        context: &str,
        binding_id: &str,
        expected_revision: Option<i64>,
        draft: &BindingDraft,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(binding_id, "sender binding ID")?;
        if binding_id == PREVIEW_BINDING_ID {
            return Err(Error::rejected(
                "email sender binding ID is reserved for previews",
            ));
        }
        reject_sender_name(draft.sender_name)?;
        if !strict_email(draft.sender_address) {
            return Err(Error::rejected(
                "email sender address exceeds its supported shape or bounds",
            ));
        }
        reject_unsubscribe_base(draft.unsubscribe_base)?;
        if let Some(connection) = draft.connection_id {
            reject_connection_id(connection)?;
        }
        // `write_tx` holds `BEGIN IMMEDIATE` across the revision check
        // + writes: a racing saver blocks on the write lock first, so
        // exactly one revision wins and the loser is stale.
        self.write_tx(|tx| {
            let current: Option<i64> = tx
                .query_opt(
                    "SELECT revision FROM app_sender_bindings WHERE context_id=? AND binding_id=?",
                    params![context, binding_id],
                    |r| r.get(0),
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            let revision = match (current, expected_revision) {
                (None, None) => 1,
                (None, Some(_)) => {
                    return Err(Error::rejected(
                        "email sender binding is unknown; save without an expected revision",
                    ));
                }
                (Some(_), None) => {
                    return Err(Error::rejected(
                        "email sender binding already exists; name the observed revision",
                    ));
                }
                (Some(current), Some(expected)) => {
                    if current != expected {
                        return Err(Error::rejected("email sender binding revision is stale"));
                    }
                    current
                        .checked_add(1)
                        .ok_or_else(|| Error::rejected("email sender binding revision exhausted"))?
                }
            };
            let digest = binding_digest(self.install(), context, binding_id, revision, draft);
            if current.is_none() {
                tx.execute(
                    "INSERT INTO app_sender_bindings(context_id,binding_id,revision,sender_name,sender_address,unsubscribe_base,connection_id,binding_digest,created,updated) VALUES(?,?,?,?,?,?,?,?,?,?)",
                    params![context, binding_id, revision, draft.sender_name, draft.sender_address, draft.unsubscribe_base, draft.connection_id, digest, now(), now()],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            } else {
                let changed = tx
                    .execute(
                        "UPDATE app_sender_bindings SET revision=?,sender_name=?,sender_address=?,unsubscribe_base=?,connection_id=?,binding_digest=?,updated=? WHERE context_id=? AND binding_id=? AND revision=?",
                        params![revision, draft.sender_name, draft.sender_address, draft.unsubscribe_base, draft.connection_id, digest, now(), context, binding_id, current],
                    )
                    .map_err(|e| Error::internal(e.to_string()))?;
                if changed != 1 {
                    return Err(Error::rejected("email sender binding revision is stale"));
                }
            }
            tx.execute(
                "INSERT INTO app_sender_binding_revisions(context_id,binding_id,revision,sender_name,sender_address,unsubscribe_base,connection_id,binding_digest,actor,at) VALUES(?,?,?,?,?,?,?,?,'operator',?)",
                params![context, binding_id, revision, draft.sender_name, draft.sender_address, draft.unsubscribe_base, draft.connection_id, digest, now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            Ok(())
        })?;
        Ok(json!({"binding": self.app_sender_binding_show(context, binding_id)?["binding"]}))
    }

    pub fn app_sender_binding_show(&self, context: &str, binding_id: &str) -> Result<Value> {
        let conn = self.conn();
        let record = self.binding_record(&conn, context, binding_id)?;
        Ok(json!({"binding": self.binding_json(context, &record)}))
    }

    /// CAD-1056: the host's own unsubscribe URL shapes for this
    /// context — the preview base and every saved binding base —
    /// so operator HTML cannot link to them.
    pub fn app_unsubscribe_endpoints(
        &self,
        context: &str,
    ) -> Result<Vec<super::app_content_html::HostEndpoint>> {
        use super::app_content_html::HostEndpoint;
        crate::proto::identifier(context, "context ID")?;
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT unsubscribe_base FROM app_sender_bindings WHERE context_id=?")
            .map_err(|e| Error::internal(e.to_string()))?;
        let bases = stmt
            .query_map(params![context], |r| r.get::<_, String>(0))
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut out: Vec<HostEndpoint> = HostEndpoint::binding(UNSUBSCRIBE_BASE)
            .into_iter()
            .collect();
        for base in bases {
            let base = base.map_err(|e| Error::internal(e.to_string()))?;
            out.extend(HostEndpoint::binding(&base));
        }
        Ok(out)
    }

    pub fn app_sender_binding_list(&self, context: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT binding_id FROM app_sender_bindings WHERE context_id=? ORDER BY binding_id LIMIT ?")
            .map_err(|e| Error::internal(e.to_string()))?;
        let found = stmt
            .query_map(params![context, BINDING_LIST_MAX as i64 + 1], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut ids = Vec::new();
        for row in found {
            ids.push(row.map_err(|e| Error::internal(e.to_string()))?);
            if ids.len() > BINDING_LIST_MAX {
                return Err(Error::internal(
                    "email sender binding listing exceeds its bound",
                ));
            }
        }
        drop(stmt);
        drop(conn);
        let mut bindings = Vec::with_capacity(ids.len());
        for id in &ids {
            bindings.push(self.app_sender_binding_show(context, id)?["binding"].clone());
        }
        Ok(json!({"bindings": bindings}))
    }

    /// Render deterministic sanitized HTML and plain text from one
    /// exact revision (current when unnamed), with sample
    /// personalization and sender material from the named binding
    /// (preview placeholders when unnamed). Every render is labelled
    /// `preview_only: true` / `send_ready: false` and is never
    /// send-ready output: saved bindings are unverified operator
    /// material, so naming one changes the bytes and the digest but
    /// never the readiness.
    pub fn app_content_render(
        &self,
        context: &str,
        campaign: &str,
        revision: Option<i64>,
        sample_first_name: Option<&str>,
        binding_id: Option<&str>,
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
        let binding = self.resolve_binding(&conn, context, binding_id)?;
        let (resolved, draft, digest) = self.draft_at(&conn, context, campaign, revision)?;
        let html = render_html(&draft, sample_first_name, &binding.view);
        let text = render_text(&draft, sample_first_name, &binding.view);
        let render_digest = material_digest(&json!({
            "domain": "cadence-app-content-render-v1",
            "content_digest": digest,
            "binding_digest": binding.digest,
            "html": html,
            "text": text,
        }));
        let unsubscribe = unsubscribe_url(&binding.view);
        Ok(json!({
            "render": {
                "campaign_id": campaign,
                "install_id": self.install(),
                "context_id": context,
                "revision": resolved,
                "content_digest": digest,
                "sample_first_name": sample_first_name,
                "binding": {
                    "binding_id": binding.binding_id,
                    "revision": binding.revision,
                    "digest": binding.digest,
                    "preview_only": binding.preview_only,
                },
                "preview_only": binding.preview_only,
                "send_ready": false,
                "sender": {"name": binding.view.sender_name, "address": binding.view.sender_address},
                "unsubscribe_url": unsubscribe,
                "html": html,
                "text": text,
                "render_digest": render_digest,
            },
        }))
    }

    /// Record a bounded operator-submitted proposal draft. Inert: the
    /// draft does not change, approval does not change, nothing
    /// sends. Bound to the observed source revision; a proposal ID
    /// replays only behind identical bytes. Attribution is honestly
    /// `operator` with `origin: operator-direct`: the operator
    /// connection proves the operator submitted it, never that an
    /// assistant produced it. `assistant` attribution arrives only
    /// through `app_content_assistant_propose` (CAD-813), which the
    /// daemon gates on a live assigned chat turn with a server-verified
    /// App binding; receipt-shaped fields stay refused on this path.
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
                "SELECT campaign_id,source_revision,content_digest,actor,origin,receipt_message,receipt_agent,receipt_request FROM app_content_proposals WHERE context_id=? AND proposal_id=?",
                params![context, proposal_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
        {
            if stored.0 == campaign && stored.1 == source_revision && stored.2 == digest
                && stored.3 == "operator" && stored.4 == "operator-direct"
                && stored.5.is_none() && stored.6.is_none() && stored.7.is_none()
            {
                drop(conn);
                return self.app_content_proposal_show(context, proposal_id);
            }
            return Err(Error::rejected("email proposal ID is already used"));
        }
        conn.execute(
            "INSERT INTO app_content_proposals(context_id,proposal_id,campaign_id,source_revision,subject,preheader,blocks,content_digest,actor,origin,state,created,decided) VALUES(?,?,?,?,?,?,?,?,'operator','operator-direct','pending',?,NULL)",
            params![context, proposal_id, campaign, source_revision, draft.subject, draft.preheader, blocks_text, digest, now()],
        )
        .map_err(|e| Error::internal(e.to_string()))?;
        drop(conn);
        self.app_content_proposal_show(context, proposal_id)
    }

    fn proposal_row(
        &self,
        conn: &impl super::StoreConn,
        context: &str,
        proposal_id: &str,
    ) -> Result<ProposalRow> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(proposal_id, "proposal ID")?;
        conn.query_row(
            "SELECT campaign_id,source_revision,subject,preheader,blocks,content_digest,actor,origin,receipt_message,receipt_agent,receipt_request,state,created,decided FROM app_content_proposals WHERE context_id=? AND proposal_id=?",
            params![context, proposal_id],
            |r| {
                Ok(ProposalRow {
                    campaign: r.get(0)?,
                    source_revision: r.get(1)?,
                    subject: r.get(2)?,
                    preheader: r.get(3)?,
                    blocks: r.get(4)?,
                    digest: r.get(5)?,
                    actor: r.get(6)?,
                    origin: r.get(7)?,
                    receipt_message: r.get(8)?,
                    receipt_agent: r.get(9)?,
                    receipt_request: r.get(10)?,
                    state: r.get(11)?,
                    created: r.get(12)?,
                    decided: r.get(13)?,
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
        let receipt = match (
            &row.receipt_message,
            &row.receipt_agent,
            &row.receipt_request,
        ) {
            (Some(message), Some(agent), Some(request)) => json!({
                "message_id": message,
                "agent": agent,
                "request_id": request,
                "install_id": self.install(),
                "context_id": context,
                "campaign_id": row.campaign,
                "source_revision": row.source_revision,
            }),
            _ => Value::Null,
        };
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
            "actor": row.actor,
            "origin": row.origin,
            "assistant_receipt": receipt,
            "state": row.state,
            "created": row.created,
            "decided": row.decided,
        }}))
    }

    /// CAD-1014: render a STORED proposal (the assistant's inert pending
    /// draft) through the exact same `render_html`/`render_text` the
    /// saved-content path uses — the operator's before-Apply preview.
    /// Pure read: no apply/save/approve/send, no doc write, no freeze.
    /// `send_ready:false`/`preview_only` always; the render carries the
    /// proposal id, its state and the source revision it was drafted
    /// against so the UI labels it as a proposal, never as live content.
    /// A proposal that is not `pending` refuses — only an unapplied draft
    /// has a preview to render.
    pub fn app_content_proposal_render(
        &self,
        context: &str,
        proposal_id: &str,
        sample_first_name: Option<&str>,
        binding_id: Option<&str>,
    ) -> Result<Value> {
        if let Some(name) = sample_first_name {
            if !sample_name_valid(name) {
                return Err(Error::rejected(
                    "email sample name exceeds its supported shape or bounds",
                ));
            }
        }
        let conn = self.conn();
        let row = self.proposal_row(&conn, context, proposal_id)?;
        if row.state != "pending" {
            return Err(Error::rejected(
                "only a pending proposal has a before-apply preview",
            ));
        }
        let raw: Value = serde_json::from_str(&row.blocks).unwrap_or(Value::Null);
        let blocks = raw
            .as_array()
            .cloned()
            .ok_or_else(|| Error::rejected("email proposal blocks are not an array"))?;
        let draft = Draft::parse(&row.subject, &row.preheader, &blocks)?;
        let binding = self.resolve_binding(&conn, context, binding_id)?;
        let html = render_html(&draft, sample_first_name, &binding.view);
        let text = render_text(&draft, sample_first_name, &binding.view);
        let render_digest = material_digest(&json!({
            "domain": "cadence-app-content-render-v1",
            "content_digest": row.digest,
            "binding_digest": binding.digest,
            "proposal_id": proposal_id,
            "html": html,
            "text": text,
        }));
        let unsubscribe = unsubscribe_url(&binding.view);
        Ok(json!({
            "render": {
                "proposal_id": proposal_id,
                "campaign_id": row.campaign,
                "install_id": self.install(),
                "context_id": context,
                "state": row.state,
                "source_revision": row.source_revision,
                "content_digest": row.digest,
                "sample_first_name": sample_first_name,
                "binding": {
                    "binding_id": binding.binding_id,
                    "revision": binding.revision,
                    "digest": binding.digest,
                    "preview_only": binding.preview_only,
                },
                "preview_only": true,
                "send_ready": false,
                "sender": {"name": binding.view.sender_name, "address": binding.view.sender_address},
                "unsubscribe_url": unsubscribe,
                "html": html,
                "text": text,
                "render_digest": render_digest,
            },
        }))
    }

    /// CAD-813: mint a one-time, host-stamped proposal request.
    /// The daemon proved the chat message carries the server-verified
    /// App binding for this installation and context before calling
    /// here, so the stamped campaign and source revision are host
    /// scope, never agent text: `source_revision` is the live draft
    /// revision read here (0 when no draft exists yet). Inert: no
    /// proposal, no draft change, no approval change. A request id
    /// replays only behind the identical stamped scope; the assistant
    /// redeems it exactly once across all proposal ids.
    pub fn app_content_proposal_request(
        &self,
        context: &str,
        campaign: &str,
        message: &str,
        request_id: &str,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(campaign, "campaign ID")?;
        crate::proto::identifier(request_id, "proposal request ID")?;
        if message.is_empty() || message.len() > 128 || message.chars().any(char::is_control) {
            return Err(Error::rejected("proposal message identity is malformed"));
        }
        let conn = self.conn();
        let source_revision = self
            .content_row(&conn, context, campaign)?
            .map(|row| row.revision)
            .unwrap_or(0);
        if let Some(stored) = conn
            .query_row(
                "SELECT campaign_id,source_revision,message_id,state,used_by FROM app_content_proposal_requests WHERE context_id=? AND request_id=?",
                params![context, request_id],
                |r| {
                    Ok(ProposalRequestRow {
                        campaign: r.get(0)?,
                        source_revision: r.get(1)?,
                        message: r.get(2)?,
                        state: r.get(3)?,
                        used_by: r.get(4)?,
                        created: 0.0,
                        decided: None,
                    })
                },
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
        {
            if stored.campaign == campaign
                && stored.source_revision == source_revision
                && stored.message == message
            {
                drop(conn);
                return self.app_content_proposal_request_show(context, request_id);
            }
            return Err(Error::rejected("email proposal request ID is already used"));
        }
        conn.execute(
            "INSERT INTO app_content_proposal_requests(context_id,request_id,campaign_id,source_revision,message_id,state,used_by,created,decided) VALUES(?,?,?,?,?,'open',NULL,?,NULL)",
            params![context, request_id, campaign, source_revision, message, now()],
        )
        .map_err(|e| {
            if is_claim_conflict(&e) {
                Error::rejected("email proposal request ID is already used")
            } else {
                Error::internal(e.to_string())
            }
        })?;
        drop(conn);
        self.app_content_proposal_request_show(context, request_id)
    }

    fn proposal_request_row(
        &self,
        conn: &impl super::StoreConn,
        context: &str,
        request_id: &str,
    ) -> Result<ProposalRequestRow> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(request_id, "proposal request ID")?;
        conn.query_row(
            "SELECT campaign_id,source_revision,message_id,state,used_by,created,decided FROM app_content_proposal_requests WHERE context_id=? AND request_id=?",
            params![context, request_id],
            |r| {
                Ok(ProposalRequestRow {
                    campaign: r.get(0)?,
                    source_revision: r.get(1)?,
                    message: r.get(2)?,
                    state: r.get(3)?,
                    used_by: r.get(4)?,
                    created: r.get(5)?,
                    decided: r.get(6)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::internal(e.to_string()))?
        .ok_or_else(|| {
            Error::rejected("email proposal request is unknown for this installation and context")
        })
    }

    pub fn app_content_proposal_request_show(
        &self,
        context: &str,
        request_id: &str,
    ) -> Result<Value> {
        let conn = self.conn();
        let row = self.proposal_request_row(&conn, context, request_id)?;
        Ok(json!({"request": {
            "request_id": request_id,
            "install_id": self.install(),
            "context_id": context,
            "campaign_id": row.campaign,
            "source_revision": row.source_revision,
            "message_id": row.message,
            "state": row.state,
            "used_by": row.used_by,
            "created": row.created,
            "decided": row.decided,
        }}))
    }

    /// CAD-813: redeem a stamped proposal request for one bounded
    /// assistant-authored proposal draft. The daemon — never the
    /// caller — proved the turn (assigned agent, live token, endpoint
    /// session) and the App binding (installation, context) before
    /// calling here, so `agent` and `message` are daemon-resolved
    /// provenance, not request fields; campaign and source revision
    /// come from the stamped request alone, never agent text. Inert
    /// like the operator path: the draft does not change, approval
    /// does not change, nothing sends. The claim is atomic under an
    /// IMMEDIATE transaction: one chat message redeems one proposal
    /// across ALL proposal ids — a fresh id on the same message
    /// refuses as already claimed, a spent request refuses, and a
    /// request whose stamped source drifted behind the live draft
    /// refuses as stale. The identical redemption (same id, bytes
    /// and provenance) replays idempotently. Attribution is honestly
    /// `assistant` with `origin: assistant-receipt`. Content
    /// validation and renderer grammar are the shared CAD-782 path:
    /// the `Draft` arrived already parsed.
    pub fn app_content_assistant_propose(
        &self,
        context: &str,
        campaign: &str,
        proposal_id: &str,
        draft: &Draft,
        claim: &AssistantClaim<'_>,
    ) -> Result<Value> {
        let agent = claim.agent;
        let message = claim.message;
        let request_id = claim.request;
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(campaign, "campaign ID")?;
        crate::proto::identifier(proposal_id, "proposal ID")?;
        // Daemon-resolved provenance, never caller authority: the
        // agent alias follows the agent grammar
        // (`[A-Za-z0-9._-]`, 1-80), the message id and request id are
        // bounded text — the daemon proved all three against its
        // store before calling here, so an unknown triple refuses on
        // lookup.
        if agent.is_empty()
            || agent.len() > 80
            || !agent
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(Error::rejected("proposal agent identity is malformed"));
        }
        if message.is_empty() || message.len() > 128 || message.chars().any(char::is_control) {
            return Err(Error::rejected("proposal message identity is malformed"));
        }
        crate::proto::identifier(request_id, "proposal request ID")?;
        let digest = proposal_digest(self.install(), context, campaign, proposal_id, draft);
        let blocks_text = serde_json::to_string(&draft.canonical_blocks())
            .map_err(|e| Error::internal(e.to_string()))?;
        // `write_tx` holds `BEGIN IMMEDIATE` across the whole redeem:
        // concurrent redeemers of one message serialize on the write
        // lock; the losers meet the spent request, the per-message
        // claim or the UNIQUE backstop below.
        self.write_tx(|tx| {
            let source_revision = tx
                .query_opt(
                    "SELECT revision FROM app_content_docs WHERE context_id=? AND campaign_id=?",
                    params![context, campaign],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(|e| Error::internal(e.to_string()))?
                .unwrap_or(0);
        // The stamped request decides scope: unknown requests,
        // requests for another message or campaign, spent requests
        // and requests whose stamped source drifted behind the live
        // draft all refuse — agent text never widens them.
            let stamped: ProposalRequestRow = tx
                .query_opt(
                    "SELECT campaign_id,source_revision,message_id,state,used_by,created,decided FROM app_content_proposal_requests WHERE context_id=? AND request_id=?",
                    params![context, request_id],
                    |r| {
                        Ok(ProposalRequestRow {
                            campaign: r.get(0)?,
                            source_revision: r.get(1)?,
                            message: r.get(2)?,
                            state: r.get(3)?,
                            used_by: r.get(4)?,
                            created: r.get(5)?,
                            decided: r.get(6)?,
                        })
                    },
                )
                .map_err(|e| Error::internal(e.to_string()))?
                .ok_or_else(|| {
                    Error::rejected(
                        "email proposal request is unknown for this installation and context",
                    )
                })?;
        if stamped.message != message {
            return Err(Error::rejected(
                "email proposal request was stamped for another chat message",
            ));
        }
        if stamped.campaign != campaign {
            return Err(Error::rejected(
                "email proposal does not match its stamped campaign",
            ));
        }
        if stamped.source_revision != source_revision {
            return Err(Error::rejected("email proposal source revision is stale"));
        }
        // Same id, identical bytes and provenance: idempotent replay
        // — checked before the spent gate so a retried redemption
        // reads back instead of refusing.
        // Same id otherwise: already used. A row under another id on
        // this message: the one claim is spent.
        #[allow(clippy::type_complexity)]
        let existing: Option<(
            String,
            i64,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            )> = tx
                .query_opt(
                    "SELECT campaign_id,source_revision,content_digest,actor,origin,receipt_message,receipt_agent,receipt_request FROM app_content_proposals WHERE context_id=? AND proposal_id=?",
                    params![context, proposal_id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                        ))
                    },
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if let Some(stored) = existing {
                if stored.0 == campaign
                    && stored.1 == source_revision
                    && stored.2 == digest
                    && stored.3 == "assistant"
                    && stored.4 == "assistant-receipt"
                    && stored.5.as_deref() == Some(message)
                    && stored.6.as_deref() == Some(agent)
                    && stored.7.as_deref() == Some(request_id)
                {
                    // Idempotent replay: commit (empty) then re-read.
                    return Ok(true);
                }
                return Err(Error::rejected("email proposal ID is already used"));
            }
            let claimed: Option<String> = tx
                .query_opt(
                    "SELECT proposal_id FROM app_content_proposals WHERE context_id=? AND receipt_message=?",
                    params![context, message],
                    |r| r.get(0),
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if claimed.is_some() {
                return Err(Error::rejected("email proposal message is already claimed"));
            }
            let spent = tx
                .execute(
                    "UPDATE app_content_proposal_requests SET state='used',used_by=?,decided=? WHERE context_id=? AND request_id=? AND state='open'",
                    params![proposal_id, now(), context, request_id],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if spent != 1 {
                return Err(Error::rejected("email proposal request is already claimed"));
            }
            if let Err(error) = tx.execute(
                "INSERT INTO app_content_proposals(context_id,proposal_id,campaign_id,source_revision,subject,preheader,blocks,content_digest,actor,origin,receipt_message,receipt_agent,receipt_request,state,created,decided) VALUES(?,?,?,?,?,?,?,?,'assistant','assistant-receipt',?,?,?,'pending',?,NULL)",
                params![context, proposal_id, campaign, source_revision, draft.subject, draft.preheader, blocks_text, digest, message, agent, request_id, now()],
            ) {
                if is_claim_conflict(&error) {
                    return Err(Error::rejected(
                        "email proposal message is already claimed",
                    ));
                }
                return Err(Error::internal(error.to_string()));
            }
            Ok(false)
        })?;
        self.app_content_proposal_show(context, proposal_id)
    }

    /// CAD-1014(b) composer-free scoped-chat draft: an assigned agent
    /// turn drafts a campaign email with NO manual mint. The host
    /// derives `source_revision` from the LIVE doc (0 when the campaign
    /// has none — a first draft), stamps `receipt_request` with the
    /// message id itself (the verified turn IS the request — there is
    /// no operator-minted request row to redeem), and claims the
    /// message via the `app_content_proposal_claim` unique index so one
    /// turn produces one draft. The proposal stays `pending` /
    /// `assistant-receipt` — never edits content, approves or sends;
    /// Apply/Discard are still the operator's. `request_id` in the
    /// provenance is the message id, honest because no mint exists.
    pub fn app_content_assistant_draft(
        &self,
        context: &str,
        campaign: &str,
        proposal_id: &str,
        draft: &Draft,
        agent: &str,
        message: &str,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(campaign, "campaign ID")?;
        crate::proto::identifier(proposal_id, "proposal ID")?;
        if agent.is_empty()
            || agent.len() > 80
            || !agent
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(Error::rejected("draft agent identity is malformed"));
        }
        if message.is_empty() || message.len() > 128 || message.chars().any(char::is_control) {
            return Err(Error::rejected("draft message identity is malformed"));
        }
        let digest = proposal_digest(self.install(), context, campaign, proposal_id, draft);
        let blocks_text = serde_json::to_string(&draft.canonical_blocks())
            .map_err(|e| Error::internal(e.to_string()))?;
        let conn = self.conn();
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| Error::internal(e.to_string()))?;
        // Host-derived source: the live draft revision, or 0 for a
        // first draft — never agent text.
        let source_revision = tx
            .query_row(
                "SELECT revision FROM app_content_docs WHERE context_id=? AND campaign_id=?",
                params![context, campaign],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
            .unwrap_or(0);
        // Idempotent replay of the same proposal id on the same
        // message+agent+identical bytes returns the stored proposal.
        let existing: Option<PriorDraft> = tx
            .query_row(
                "SELECT campaign_id,source_revision,content_digest,receipt_message,receipt_agent FROM app_content_proposals WHERE context_id=? AND proposal_id=?",
                params![context, proposal_id],
                |r| {
                    Ok(PriorDraft {
                        campaign_id: r.get(0)?,
                        source_revision: r.get(1)?,
                        content_digest: r.get(2)?,
                        receipt_message: r.get(3)?,
                        receipt_agent: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?;
        if let Some(stored) = existing {
            if stored.campaign_id == campaign
                && stored.source_revision == source_revision
                && stored.content_digest == digest
                && stored.receipt_message.as_deref() == Some(message)
                && stored.receipt_agent.as_deref() == Some(agent)
            {
                drop(tx);
                drop(conn);
                return self.app_content_proposal_show(context, proposal_id);
            }
            return Err(Error::rejected("email draft proposal ID is already used"));
        }
        // One turn = one draft: the unique claim index on
        // receipt_message refuses a second proposal for this message.
        if let Err(error) = tx.execute(
            "INSERT INTO app_content_proposals(context_id,proposal_id,campaign_id,source_revision,subject,preheader,blocks,content_digest,actor,origin,receipt_message,receipt_agent,receipt_request,state,created,decided) VALUES(?,?,?,?,?,?,?,?,'assistant','assistant-receipt',?,?,?,'pending',?,NULL)",
            params![context, proposal_id, campaign, source_revision, draft.subject, draft.preheader, blocks_text, digest, message, agent, message, now()],
        ) {
            if is_claim_conflict(&error) {
                return Err(Error::rejected(
                    "this scoped chat message already produced a draft",
                ));
            }
            return Err(Error::internal(error.to_string()));
        }
        tx.commit().map_err(|e| Error::internal(e.to_string()))?;
        drop(conn);
        self.app_content_proposal_show(context, proposal_id)
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
        drop(conn);
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
            html: None,
            text: None,
            name: None,
        };
        // `write_tx` holds `BEGIN IMMEDIATE` across the revision check
        // + writes, so a racing apply blocks on the write lock first.
        // `conn` (the read guard) must be released before it re-locks
        // the same mutex.
        drop(conn);
        self.write_tx(|tx| {
        let current: Option<i64> = tx
            .query_opt(
                "SELECT revision FROM app_content_docs WHERE context_id=? AND campaign_id=?",
                params![context, proposal.campaign],
                |r| r.get(0),
            )
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
                    "UPDATE app_content_docs SET revision=?,subject=?,preheader=?,blocks=?,content_digest=?,approval_revision=NULL,approval_digest=NULL,actor='operator',updated=?,html=NULL,text_override=NULL WHERE context_id=? AND campaign_id=? AND revision=?",
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
        Ok(())
        })?;
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

    /// Operator approval pins the exact current content revision and
    /// digest — content only. Audience freezes and sender bindings
    /// are named explicitly at send-preparation time and digest-bound
    /// there; rotating either changes the send digest while this
    /// approval stands. Any later save or apply clears the pin, so a
    /// stale approval never reads as current.
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

    fn send_payload(&self, conn: &impl super::StoreConn, prep: &SendPrep) -> Result<Value> {
        let context = prep.context;
        let campaign = prep.campaign;
        let kind = prep.kind;
        let binding = prep.binding;
        let (revision, draft, digest) = self.draft_at(conn, context, campaign, None)?;
        let html = render_html(&draft, None, &binding.view);
        let text = render_text(&draft, None, &binding.view);
        let audience_digest = match prep.audience_freeze_id {
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
        let unsubscribe = unsubscribe_url(&binding.view);
        // Test and final preparation share this exact shape and the
        // same renderer. The binding digest is frozen into the
        // payload: rotating the sender binding changes the send
        // digest while the content digest — and content approval —
        // stand independent. Substituting frozen bytes outside this
        // digest (a different sender, a real unsubscribe token) is a
        // different payload, never a silent edit. In this ticket every
        // payload is preview-only (`preview_only: true`,
        // `send_ready: false`): no verified binding exists yet.
        let payload = json!({
            "kind": kind,
            "campaign_id": campaign,
            "install_id": self.install(),
            "context_id": context,
            "content_revision": revision,
            "content_digest": digest,
            "audience_freeze_id": prep.audience_freeze_id,
            "audience_digest": audience_digest,
            "sender_binding": {
                "binding_id": binding.binding_id,
                "revision": binding.revision,
                "digest": binding.digest,
                "connection_id": binding.connection_id,
                "preview_only": binding.preview_only,
            },
            "preview_only": true,
            "send_ready": false,
            "sender": {"name": binding.view.sender_name, "address": binding.view.sender_address},
            "unsubscribe_url": unsubscribe,
            "headers": {
                "List-Unsubscribe": format!("<{unsubscribe}>"),
                "List-Unsubscribe-Post": "List-Unsubscribe=One-Click",
            },
            "to_email": prep.to_email,
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
                "binding_digest": binding.digest,
                "to_email": prep.to_email,
                "html": html,
                "text": text,
            })),
        });
        Ok(payload)
    }

    /// Test-send preparation for one operator address: frozen content
    /// hash, same renderer as final send, sender material from the
    /// named binding (preview placeholders when unnamed, always
    /// labelled `preview_only: true`, never send-ready). No SMTP
    /// submission happens here (CAD-785/786 own delivery).
    pub fn app_content_test_prepare(
        &self,
        context: &str,
        campaign: &str,
        to_email: &str,
        binding_id: Option<&str>,
    ) -> Result<Value> {
        // Operator-typed recipients are never pasted HTML.
        if !strict_email(to_email) {
            return Err(Error::rejected(
                "email test recipient exceeds its supported shape or bounds",
            ));
        }
        let conn = self.conn();
        let binding = self.resolve_binding(&conn, context, binding_id)?;
        Ok(
            json!({"test_send": self.send_payload(&conn, &SendPrep { context, campaign, kind: "test", to_email: Some(to_email), audience_freeze_id: None, binding: &binding })?}),
        )
    }

    /// Verified test-send bytes for CAD-785: the exact current
    /// revision rendered through the same deterministic
    /// HTML/text renderer, but with the host-custodied verified
    /// sender — never the `.invalid` preview placeholders. The
    /// unsubscribe material stays the preview base: CAD-786 owns
    /// unsubscribe authority and per-recipient tokens, so the
    /// receipt labels it accordingly. No SMTP submission happens
    /// here; the CAD-785 daemon RPC performs it under the live
    /// installation/context link's authority.
    pub fn app_content_verified_test_bytes(
        &self,
        context: &str,
        campaign: &str,
        sender_name: &str,
        sender_address: &str,
    ) -> Result<Value> {
        let conn = self.conn();
        let (revision, draft, digest) = self.draft_at(&conn, context, campaign, None)?;
        let view = BindingView {
            sender_name: sender_name.to_string(),
            sender_address: sender_address.to_string(),
            unsubscribe_base: UNSUBSCRIBE_BASE.to_string(),
            unsubscribe_url: None,
        };
        let html = render_html(&draft, None, &view);
        let text = render_text(&draft, None, &view);
        let binding_digest = material_digest(&json!({
            "domain": "cadence-crm-smtp-sender-v1",
            "install_id": self.install(),
            "context_id": context,
            "sender_name": sender_name,
            "sender_address": sender_address,
        }));
        let payload_digest = material_digest(&json!({
            "domain": "cadence-app-content-send-v1",
            "kind": "test",
            "install_id": self.install(),
            "context_id": context,
            "campaign_id": campaign,
            "content_digest": digest,
            "audience_digest": Value::Null,
            "binding_digest": binding_digest,
            "to_email": Value::Null,
            "html": html,
            "text": text,
        }));
        Ok(json!({
            "subject": draft.subject,
            "content_revision": revision,
            "content_digest": digest,
            "binding_digest": binding_digest,
            "payload_digest": payload_digest,
            "html": html,
            "text": text,
            "unsubscribe_url": unsubscribe_url(&view),
        }))
    }

    /// Final-send preparation always refuses in this ticket: no
    /// verified sender binding exists yet. A named saved binding only
    /// proves the operator typed sender bytes — never a host-custodied
    /// CAD-785 connection, a verified sender, or CAD-786 unsubscribe
    /// authority — and domain syntax is not verification, so a
    /// real-looking address, an absent connection_id and a fictitious
    /// connection_id all refuse identically. Unknown bindings refuse
    /// as unavailable before this gate. CAD-785/786 supply the trusted
    /// evidence and the refusal lifts there. Still no SMTP submission.
    pub fn app_content_send_prepare(
        &self,
        context: &str,
        _campaign: &str,
        binding_id: &str,
        _audience_freeze_id: Option<&str>,
    ) -> Result<Value> {
        let conn = self.conn();
        let _binding = self.resolve_binding(&conn, context, Some(binding_id))?;
        Err(Error::rejected(
            "email final-send preparation moved to the operator `crm_send_prepare` verb — the approved bounded send path owns it",
        ))
    }

    /// Per-recipient bytes for the CAD-786 worker: the exact current
    /// revision rendered with the host-custodied verified sender,
    /// this recipient's first-name token and the real per-recipient
    /// unsubscribe URL. The caller proves approval separately
    /// (`app_content_approved`); this render still refuses if the
    /// revision/digest moved — a stale send renders nothing new.
    pub fn app_content_send_bytes(
        &self,
        context: &str,
        campaign: &str,
        sender_name: &str,
        sender_address: &str,
        first_name: Option<&str>,
        unsubscribe_url: &str,
    ) -> Result<Value> {
        let conn = self.conn();
        let (revision, draft, digest) = self.draft_at(&conn, context, campaign, None)?;
        let view = BindingView {
            sender_name: sender_name.to_string(),
            sender_address: sender_address.to_string(),
            unsubscribe_base: UNSUBSCRIBE_BASE.to_string(),
            unsubscribe_url: Some(unsubscribe_url.to_string()),
        };
        let html = render_html(&draft, first_name, &view);
        let text = render_text(&draft, first_name, &view);
        Ok(json!({
            "subject": personalize(&draft.subject, first_name),
            "content_revision": revision,
            "content_digest": digest,
            "html": html,
            "text": text,
            "unsubscribe_url": unsubscribe_url,
        }))
    }
}

impl Store {
    /// Best-effort audit event for a content write that already
    /// committed inside its installation file. Advisory like
    /// `note_app_audience`: digests only, never content.
    pub fn note_app_content(&self, install: &str, context: &str, action: &str, digest: &str) {
        self.note_app_content_by(install, context, action, digest, "operator");
    }

    /// CAD-813: the same advisory event with an explicit actor — the
    /// assistant turn's agent alias for verified proposals. Digests
    /// only, never content.
    pub fn note_app_content_by(
        &self,
        install: &str,
        context: &str,
        action: &str,
        digest: &str,
        actor: &str,
    ) {
        if let Err(e) = self.write_tx(|tx| Self::event(&*tx, Self::DAEMON_STREAM,
            "app_content_changed",
            json!({"install_id": install, "context_id": context, "action": action, "digest": digest, "actor": actor}),)) {
            eprintln!("store: best-effort app content audit failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_draft() -> Draft {
        Draft::parse(
            "Welcome {{first_name|friend}}",
            "A note",
            &[
                json!({"type": "heading", "text": "Hello {{first_name|friend}}"}),
                json!({"type": "paragraph", "text": "First line.\nSecond line."}),
                json!({"type": "button", "label": "Open", "url": "https://example.com/x"}),
            ],
        )
        .unwrap()
    }

    /// CAD-1014: the email content column is responsive — `width="100%"`
    /// with a `max-width:600px` style cap — so the narrow-390 CRM preview
    /// frame shows the whole message instead of clipping a fixed 600px
    /// table. No `width="600"` attribute ever returns.
    #[test]
    fn render_html_content_column_is_responsive() {
        let html = render_html(&test_draft(), Some("Amina"), &preview_binding_view());
        assert!(
            !html.contains("width=\"600\""),
            "fixed 600px content table clipped the narrow preview: {html}"
        );
        assert!(
            html.contains("<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#ffffff;max-width:600px;margin:24px auto;\">"),
            "responsive content column missing: {html}"
        );
        // Content still renders through the new table shape.
        assert!(html.contains("Hello Amina"), "{html}");
        assert!(html.contains("https://example.com/x"), "{html}");
    }

    /// CAD-1056: the digest binds the HTML body and the text override,
    /// so approval can never carry across a changed body.
    #[test]
    fn digest_binds_html_and_text_override() {
        let digest = |draft: &Draft| content_digest("i", "c", "k", 1, draft);
        let a = Draft::parse_html("Hi", "", "<p>one</p>").unwrap();
        let b = Draft::parse_html("Hi", "", "<p>two</p>").unwrap();
        assert_ne!(digest(&a), digest(&b));
        let with_text = a.clone().with_text(Some("plain")).unwrap();
        assert_ne!(digest(&a), digest(&with_text));
        assert_ne!(
            digest(&with_text),
            digest(&a.clone().with_text(Some("other")).unwrap())
        );
    }
}
