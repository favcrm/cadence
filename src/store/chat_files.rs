//! CAD-1168 slice 2: host-custodied chat attachments.
//!
//! A retained file the operator attaches to a `thread_send` message.
//! Bytes live content-addressed under `<workspace>/.cadence/chat-files/<sha256>`,
//! opened and written only through directory descriptors (Linux; other
//! platforms refuse the upload). This table is the metadata index — the daemon-minted `id` is the
//! only handle any caller ever sees, so no path, name or hash is a
//! reference the client supplies at read or send time.
//!
//! The caller's staging path is only a name under the server-owned
//! `<state>/wiki-uploads/` directory: the source is read through that
//! directory's own descriptor (never through the caller's alias), and
//! the private custody stage is an anonymous `O_TMPFILE` inode that
//! only this call can see.
//!
//! Scope rules:
//! - upload is the operator's alone (the board route is OperatorOnly and
//!   the RPC re-proves `operator_chat`). A home upload stays `home`/``;
//!   an app upload names a verified installation, context and existing
//!   conversation and is stored as `app:<install>@<conversation>` with
//!   the proven context — the one encoding in [`ChatFile::app_scope`];
//! - send-time resolution carries the metadata rows onto the entry's
//!   payload — a home send references genuine `home` rows only, and an
//!   app-bound send must match the exact stored app scope of its
//!   verified binding (see `thread_attachments`);
//! - read for the master is bound to the running turn's own persisted
//!   `payload.attachments` envelope and re-proves the stored scope
//!   against `message_app` + `message_conversation`
//!   (daemon/chat_files_rpc.rs); the operator path re-proves the stored
//!   scope too, never bypassing it because the caller is the operator.
//!   A `home` row is never relabeled as an app row, and a stored label
//!   that is neither `home` nor a well-formed app scope refuses.

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::{now, Store, StoreConn};
use crate::error::{Error, Result};

/// The schema (v37): one row per retained logical attachment.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS chat_files(
    id TEXT PRIMARY KEY,
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL,
    mime TEXT NOT NULL,
    name TEXT NOT NULL,
    scope TEXT NOT NULL,
    context_id TEXT NOT NULL DEFAULT '',
    uploader TEXT NOT NULL,
    created REAL NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS chat_files_sha_scope
    ON chat_files(sha256, scope, context_id);
";

/// The account registry introduced in schema v38, separate from the shipped v37 DDL.
pub(crate) const ACCOUNT_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS chat_upload_accounts(
    id TEXT PRIMARY KEY,
    principal_hash TEXT NOT NULL UNIQUE,
    authority TEXT NOT NULL,
    created REAL NOT NULL);
";

/// Custody leaf under `<workspace>/.cadence/`: `chat-files/`.
pub const CHAT_FILES_DIR: &str = "chat-files";

/// One file is at most 10 MiB (the mock's ceiling; the multipart
/// envelope rides above it at the board). A raster image is additionally
/// capped at [`crate::attachment_image::IMAGE_MAX_BYTES`] because that is
/// the native image prompt bound; a larger image refuses rather than
/// being retained as an attachment no turn can deliver.
pub const CHAT_FILE_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// A text read returns at most this many bytes (the thread's own cap).
pub const CHAT_FILE_TEXT_CAP: usize = 64_000;
/// At most this many attachments ride one message.
pub const CHAT_FILE_MAX_PER_MESSAGE: usize = 5;
/// The reserved Home scope label — the operator's own chat, no
/// installation provenance. Legacy rows all carry it.
pub const CHAT_FILE_SCOPE_HOME: &str = "home";

/// One retained attachment row.
#[derive(Debug, Clone)]
pub struct ChatFile {
    pub id: String,
    pub sha256: String,
    pub size: u64,
    pub mime: String,
    pub name: String,
    pub scope: String,
    pub context_id: String,
    pub uploader: String,
    /// New-format authenticated owner; `None` on immutable legacy rows.
    pub user_id: Option<String>,
    /// Verified native conversation id; `None` on legacy rows.
    pub thread_id: Option<String>,
    /// Custody layout version; `None` selects the checked legacy roots only.
    pub location_version: Option<i64>,
    pub created: f64,
}

impl ChatFile {
    /// The one encoding of trusted app provenance in `chat_files.scope`:
    /// `app:<install>@<conversation>`. Both id grammars exclude `:` and
    /// `@`, so the tuple is unambiguous; the proven context rides
    /// `chat_files.context_id`. Callers never build this string by hand.
    pub fn app_scope(install: &str, conversation: &str) -> Result<String> {
        if install.is_empty()
            || install.len() > 128
            || !install
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Error::rejected("bad app scope: installation id"));
        }
        if conversation.is_empty()
            || conversation.len() > 64
            || !conversation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Error::rejected("bad app scope: conversation id"));
        }
        Ok(format!("app:{install}@{conversation}"))
    }

    /// Decode a stored `scope` value: `Ok(None)` for the legacy Home
    /// label, `Ok(Some((install, conversation)))` for a well-formed app
    /// scope, and a refusal for anything else — an unknown or malformed
    /// stored label is never silently treated as Home, and app ownership
    /// is never inferred from an id.
    pub fn parse_scope(scope: &str) -> Result<Option<(&str, &str)>> {
        if scope == CHAT_FILE_SCOPE_HOME {
            return Ok(None);
        }
        let rest = scope
            .strip_prefix("app:")
            .ok_or_else(|| Error::rejected("the retained row carries an unknown scope label"))?;
        let (install, conversation) = rest
            .split_once('@')
            .ok_or_else(|| Error::rejected("the retained row carries a malformed app scope"))?;
        if Self::app_scope(install, conversation).is_err() {
            return Err(Error::rejected(
                "the retained row carries a malformed app scope",
            ));
        }
        Ok(Some((install, conversation)))
    }

    /// Refuse unless this is a genuine Home row — the legacy rows and
    /// every operator upload that named no app. App data is never
    /// relabeled Home, and a Home row never carries a context.
    /// New-format ownership must match both the authenticated account and
    /// the exact verified native conversation. Legacy rows never match.
    pub fn operational_scope_matches(&self, user_id: &str, thread_id: &str) -> Result<()> {
        if self.user_id.as_deref() != Some(user_id)
            || self.thread_id.as_deref() != Some(thread_id)
            || self.location_version != Some(1)
            || !valid_upload_account_id(user_id)
            || !valid_native_thread_id(thread_id)
        {
            return Err(Error::rejected(
                "attachment is not bound to this authenticated account and conversation",
            ));
        }
        Ok(())
    }

    pub fn home_scope(&self) -> Result<()> {
        if self.scope != CHAT_FILE_SCOPE_HOME || !self.context_id.is_empty() {
            return Err(Error::rejected(format!(
                "attachment '{}' is not a home-scope file — an app-scoped row cannot \
                 ride the home thread",
                self.id
            )));
        }
        Ok(())
    }

    /// Refuse unless this is exactly the row of that trusted app binding:
    /// its scope decodes to this installation and conversation, and its
    /// context is exactly the proven one — never defaulted, never the
    /// conversation's creation context standing in for a missing one.
    pub fn scope_matches(&self, install: &str, context: &str, conversation: &str) -> Result<()> {
        let scope = Self::app_scope(install, conversation)?;
        if self.scope != scope || self.context_id != context {
            return Err(Error::rejected(format!(
                "attachment '{}' is not scoped to this installation, context and \
                 conversation",
                self.id
            )));
        }
        Ok(())
    }

    /// What a thread entry's `payload.attachments` row carries — the
    /// metadata the board renders and the master resolves; never a path.
    pub fn ref_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "size": self.size,
            "mime": self.mime,
            "sha256": self.sha256,
        })
    }

    /// The full row for `chat_file_read` metadata / upload receipts.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "sha256": self.sha256,
            "size": self.size,
            "mime": self.mime,
            "name": self.name,
            "scope": self.scope,
            "context_id": self.context_id,
            "uploader": self.uploader,
            "user_id": self.user_id,
            "thread_id": self.thread_id,
            "location_version": self.location_version,
            "created": self.created,
        })
    }

    /// The read projection of one row from the exact bytes whose size
    /// and digest were verified against it: bounded secret-scanned UTF-8
    /// text for text/SVG/PDF, or an explicit no-text-projection receipt
    /// for a raster image. The caller passes the checked bytes; this
    /// helper never
    /// opens a path, so a completed scope or capability proof is not
    /// undone by a second read.
    pub fn read_projection(&self, bytes: Vec<u8>) -> Result<Value> {
        if visual_kind(&self.mime) && !text_kind(&self.mime) {
            // A raster image has no text projection. The model observes
            // it as native pixels through the turn image path; the read
            // verb reports exactly that instead of inventing a
            // description or returning encoded bytes.
            prove_declared_kind(&self.name, &self.mime, &bytes)?;
            checked_attachment_body(&self.name, &self.mime, &bytes)
                .map_err(|e| Error::rejected(format!("attachment '{}': {e}", self.id)))?;
            return Ok(json!({
                "id": self.id,
                "name": self.name,
                "size": self.size,
                "mime": self.mime,
                "sha256": self.sha256,
                "extractable": false,
                "visual": true,
                "delivery": "this image is delivered to an image-capable model as native image pixels; it has no text projection",
            }));
        }
        if !text_kind(&self.mime) {
            return Err(Error::rejected(format!(
                "attachment '{}' is not a text attachment",
                self.id
            )));
        }
        prove_declared_kind(&self.name, &self.mime, &bytes)?;
        let text = checked_attachment_text(&self.name, &self.mime, &bytes)?;
        let (text, truncated) = if text.len() > CHAT_FILE_TEXT_CAP {
            let mut cut = super::take_bytes(&text, CHAT_FILE_TEXT_CAP.saturating_sub(16));
            cut.push_str(" …[truncated]");
            (cut, true)
        } else {
            (text, false)
        };
        Ok(json!({
            "id": self.id,
            "name": self.name,
            "size": self.size,
            "mime": self.mime,
            "sha256": self.sha256,
            "extractable": true,
            "truncated": truncated,
            "text": text,
        }))
    }
}

/// Explicit storage authority passed by native callers. Grouping the two
/// roots keeps daemon adapter signatures clear without hiding authority in a flag.
pub struct ChatFileStorageRoots<'a> {
    pub state_dir: &'a Path,
    /// Legacy PM-rooted custody used only by compatibility APIs.
    pub workspace_dir: &'a Path,
}

/// Explicit versioned custody roots for new writes and checked reads. Legacy
/// bytes remain rooted at the daemon's PM directory; new bytes are rooted at
/// the resolved operational workspace.
pub struct ChatFileOperationalRoots<'a> {
    pub state_dir: &'a Path,
    pub legacy_workspace_dir: &'a Path,
    pub workspace_dir: &'a Path,
}

/// The daemon-minted id grammar — `chf-` + 32 lowercase hex. Checked at
/// every caller-supplied boundary so a forged id never reaches a path.
pub fn valid_upload_account_id(id: &str) -> bool {
    id.len() == 36
        && id.starts_with("acu-")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn valid_native_thread_id(id: &str) -> bool {
    crate::proto::identifier(id, "conversation ID").is_ok()
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.starts_with("chf-")
        && id[4..]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Display basename: no path segments, no control characters, bounded.
/// The client's `name` is a hint only — this is the name that is stored
/// and shown; anything unsafe is rewritten, never trusted.
pub fn sanitize_name(name: &str) -> Result<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    let clean: String = base
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cut: String = clean.chars().take(120).collect();
    if cut.is_empty() || cut == "." || cut == ".." {
        return Err(Error::rejected(
            "chat_file_upload: the file needs a usable name",
        ));
    }
    Ok(cut)
}

/// Extension (lowercase, without the dot) the name implies — "" when
/// none. Only the allowlisted set may be claimed.
fn claimed_ext(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// The first 8192 bytes, trimmed back to a UTF-8 boundary when the cut
/// falls inside a multibyte character (`error_len() == None`: the head
/// merely ends mid-char). Genuinely invalid bytes are left in, so the
/// sniff still calls them binary; the full-file decode stays the check.
fn sniff_head(bytes: &[u8]) -> &[u8] {
    let head = &bytes[..bytes.len().min(8192)];
    match std::str::from_utf8(head) {
        Err(e) if e.error_len().is_none() => &head[..e.valid_up_to()],
        _ => head,
    }
}

/// What the real bytes sniff as: text (including readable HTML and script
/// source), PDF, SVG, PNG/JPEG/WebP, or the refusal classes (executables,
/// archives, zip). A NUL in the head marks binary content even when the
/// bytes are valid UTF-8. `sniff_mime` is the wiki's magic-byte table.
fn sniffed_kind(bytes: &[u8]) -> &'static str {
    // A UTF-8 BOM is legal in an SVG/XML document and is not whitespace
    // to the wiki sniff; strip it for the shape check only.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match crate::wiki::sniff_mime(bytes) {
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" => "jpeg",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        // `sniff_mime` folds UTF-8 text to text/plain and readable HTML to
        // text/html. Both are inert UTF-8 source here; a NUL in the head
        // still marks binary content.
        "text/plain" | "text/html" if !bytes.contains(&0) => "text",
        "text/plain" | "text/html" => "binary",
        other => other,
    }
}

/// The structured-text extensions, mapped to the MIME the board records.
/// All of them are UTF-8 text, so the write-time decode and secret scan are
/// the whole validation; nothing here is parsed by format.
const TEXT_EXTS: [(&str, &str); 10] = [
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("json", "application/json"),
    ("jsonl", "application/x-ndjson"),
    ("yaml", "application/yaml"),
    ("yml", "application/yaml"),
    ("xml", "application/xml"),
];

/// The raster kinds delivered as native image pixels, keyed by the
/// extension a name may claim. The bytes must still sniff as the same
/// kind; an image with no recognised extension is accepted from its
/// bytes alone (the composer has no extension prefilter).
const IMAGE_EXTS: [(&str, &str); 4] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
];

fn raster_kind_name(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpeg",
        "image/webp" => "webp",
        _ => "",
    }
}

/// Extensions that are never attachable even when their bytes happen to
/// be valid UTF-8: executables, archives and media containers. The
/// composer has no prefilter, so this is the daemon's honest minimum —
/// the full-byte UTF-8/kind check remains the enforcement boundary for
/// everything else.
const REFUSED_EXTS: [&str; 30] = [
    "exe", "dll", "so", "dylib", "bin", "com", "msi", "msp", "wasm", "class", "jar", "zip", "tar",
    "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "iso", "dmg", "deb", "rpm", "apk", "aab", "bmp",
    "ico", "tif", "tiff",
];

/// Media/container extensions the daemon refuses by name even after the
/// visual kinds above were considered (the image decoder cannot speak
/// them, and retaining them would imply a delivery it cannot make).
const REFUSED_MEDIA_EXTS: [&str; 13] = [
    "heic", "heif", "avif", "mp4", "m4v", "mov", "avi", "mkv", "webm", "mp3", "wav", "flac", "ogg",
];

/// The claimed extension must match the real bytes; `mime` is the
/// stored content type. Refuses binary/off-list content (GIF, archives,
/// executables, corrupt/empty magic). HTML and script source are inert UTF-8
/// text, never rendered or executed. A name with no recognised
/// extension is classified from its bytes alone, so the composer needs
/// no extension prefilter: ordinary UTF-8 text, SVG source, PDF, PNG,
/// JPEG and WebP are attachable, and everything else refuses.
fn check_kind(name: &str, bytes: &[u8]) -> Result<&'static str> {
    let ext = claimed_ext(name);
    let ext = ext.as_str();
    let sniffed = sniffed_kind(bytes);
    if ext == "gif" || sniffed == "image/gif" {
        return Err(Error::rejected(format!(
            "chat_file_upload refused '{name}': GIF is not a supported visual kind — \
             use PNG, JPEG, WebP or SVG"
        )));
    }
    if REFUSED_EXTS.contains(&ext) || REFUSED_MEDIA_EXTS.contains(&ext) {
        return Err(Error::rejected(format!(
            "chat_file_upload refused '{name}': '.{ext}' is not an attachable kind — \
             expected UTF-8 text, SVG, PDF, PNG, JPEG or WebP"
        )));
    }
    if let Some((_, mime)) = IMAGE_EXTS.iter().find(|(candidate, _)| *candidate == ext) {
        if sniffed != raster_kind_name(mime) {
            return Err(Error::rejected(format!(
                "chat_file_upload refused '{name}': a '.{ext}' file must sniff as \
                 its declared kind — the bytes did not (extension and client MIME \
                 are never the enforcement boundary)"
            )));
        }
        return Ok(mime);
    }
    if ext == "svg" {
        if sniffed != "svg" {
            return Err(Error::rejected(format!(
                "chat_file_upload refused '{name}': a '.svg' file must sniff as \
                 its declared kind — the bytes did not (extension and client MIME \
                 are never the enforcement boundary)"
            )));
        }
        return Ok("image/svg+xml");
    }
    if ext == "pdf" {
        if sniffed != "pdf" {
            return Err(Error::rejected(format!(
                "chat_file_upload refused '{name}': a '.pdf' file must sniff as \
                 its declared kind — the bytes did not (extension and client MIME \
                 are never the enforcement boundary)"
            )));
        }
        return Ok("application/pdf");
    }
    if let Some((_, mime)) = TEXT_EXTS.iter().find(|(candidate, _)| *candidate == ext) {
        if sniffed != "text" {
            return Err(Error::rejected(format!(
                "chat_file_upload refused '{name}': a '.{ext}' file must sniff as \
                 its declared kind — the bytes did not (extension and client MIME \
                 are never the enforcement boundary)"
            )));
        }
        return Ok(mime);
    }
    // No recognised extension: the bytes decide. This is what lets the
    // operator pick any file and have the daemon classify it.
    match sniffed {
        "text" => Ok("text/plain"),
        "svg" => Ok("image/svg+xml"),
        "png" => Ok("image/png"),
        "jpeg" => Ok("image/jpeg"),
        "webp" => Ok("image/webp"),
        "pdf" => Ok("application/pdf"),
        other => Err(Error::rejected(format!(
            "chat_file_upload refused '{name}': the bytes sniff as '{other}', which \
             is not an attachable kind — expected UTF-8 text, SVG, PDF, PNG, JPEG \
             or WebP"
        ))),
    }
}

/// The extracted text of one retained attachment, from the exact bytes the
/// caller already verified against the row's size and digest. Text kinds are
/// their own text; a PDF is read through the same bounded `pdftotext` seam the
/// wiki ingest uses, so an unreadable or text-free PDF refuses instead of
/// becoming an attachment the assistant cannot read.
fn attachment_text(name: &str, mime: &str, bytes: &[u8]) -> Result<String> {
    if mime != "application/pdf" {
        if !text_kind(mime) {
            return Err(Error::rejected(format!(
                "attachment '{name}' is not a text source"
            )));
        }
        if bytes.contains(&0) {
            return Err(Error::rejected(format!(
                "chat_file_upload '{name}': the bytes contain NUL — that is binary \
                 content, not text"
            )));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|e| {
                Error::rejected(format!("chat_file_upload '{name}': not UTF-8 text: {e}"))
            })?
            .to_string();
        // SVG source is retained and read as inert text. Rendering it is a
        // separate proof: `attachment_image::rasterize_svg` parses the source
        // with the linked XML parser and refuses it before any renderer runs
        // when it carries script, markup, animation or an external reference.
        return Ok(text);
    }
    pdf_attachment_text(PDFTOTEXT, name, bytes)
}

/// The one content gate every retained kind passes at upload and at
/// every readiness check: text/SVG extract and scan; raster images prove
/// their signature and decode within the delivery bound. Only those
/// kinds are reachable here — `check_kind` has already classified the
/// bytes.
fn checked_attachment_body(name: &str, mime: &str, bytes: &[u8]) -> Result<()> {
    if text_kind(mime) {
        checked_attachment_text(name, mime, bytes)?;
        return Ok(());
    }
    if crate::attachment_image::raster_mime(mime) {
        return crate::attachment_image::validate_raster(bytes, mime)
            .map_err(|e| Error::rejected(format!("chat_file_upload refused '{name}': {e}")));
    }
    Err(Error::rejected(format!(
        "chat_file_upload refused '{name}': '{mime}' is not a supported attachment kind"
    )))
}

/// The converter a PDF attachment is read through (poppler-utils).
const PDFTOTEXT: &str = "pdftotext";

/// Read a PDF through `program`. A converter that cannot start is a missing
/// install, said as such with its remedy — never reported as a bad PDF.
fn pdf_attachment_text(program: &str, name: &str, bytes: &[u8]) -> Result<String> {
    use std::io::Write;
    let mut staged = tempfile::NamedTempFile::new().map_err(|e| {
        Error::rejected(format!(
            "chat_file_upload '{name}': staging the PDF failed: {e}"
        ))
    })?;
    staged
        .write_all(bytes)
        .and_then(|()| staged.flush())
        .map_err(|e| {
            Error::rejected(format!(
                "chat_file_upload '{name}': staging the PDF failed: {e}"
            ))
        })?;
    let (output, bounds) = crate::proc::run_bounded_limited(
        std::process::Command::new(program)
            .args(["-enc", "UTF-8", "-nopgbrk"])
            .arg(staged.path())
            .arg("-"),
        std::time::Duration::from_secs(20),
        CHAT_FILE_TEXT_CAP,
    )
    .map_err(|e| match e {
        crate::proc::BoundedError::Spawn(ref io) if io.kind() == std::io::ErrorKind::NotFound => {
            Error::rejected(format!(
                "chat_file_upload refused '{name}': PDF text extraction is unavailable on \
                 this host because `{program}` is not installed (install poppler-utils); \
                 the PDF itself was not judged"
            ))
        }
        e => Error::rejected(format!(
            "chat_file_upload refused '{name}': the PDF could not be read: {e}"
        )),
    })?;
    if bounds.stdout_exceeded {
        return Err(Error::rejected(format!(
            "chat_file_upload refused '{name}': the PDF's text exceeds the {CHAT_FILE_TEXT_CAP}-byte cap"
        )));
    }
    if !output.status.success() {
        return Err(Error::rejected(format!(
            "chat_file_upload refused '{name}': the PDF is corrupt, encrypted or \
             otherwise unreadable"
        )));
    }
    let text = String::from_utf8(output.stdout).map_err(|_| {
        Error::rejected(format!(
            "chat_file_upload refused '{name}': the PDF's text is not UTF-8"
        ))
    })?;
    if text.trim().is_empty() {
        return Err(Error::rejected(format!(
            "chat_file_upload refused '{name}': the PDF has no extractable text — \
             a scanned or image-only PDF needs OCR, which is not available yet"
        )));
    }
    Ok(text)
}

/// A row's declared MIME must still match what its bytes sniff as. Upload
/// proves it once; readiness and every read prove it again, because a row is
/// reached through paths that do not share the readiness helper — the Home
/// operator read goes straight from the digest check to `read_projection`.
fn prove_declared_kind(name: &str, mime: &str, bytes: &[u8]) -> Result<()> {
    let declared = check_kind(name, sniff_head(bytes)).map_err(|e| {
        Error::rejected(format!(
            "the '{name}' attachment's bytes do not match its declared kind: {e}"
        ))
    })?;
    if declared != mime {
        return Err(Error::rejected(format!(
            "the '{name}' attachment declares '{mime}' but its bytes sniff as '{declared}'"
        )));
    }
    Ok(())
}

/// The one gate an attachment's text passes before it is retained, before a
/// send references it, and before a read returns it: extract, then scan for
/// credentials. The same bytes therefore produce the same verdict at every
/// boundary.
fn checked_attachment_text(name: &str, mime: &str, bytes: &[u8]) -> Result<String> {
    let text = attachment_text(name, mime, bytes)?;
    crate::secret::guard(&format!("chat-file/{name}"), &text)?;
    Ok(text)
}

/// `true` for the kinds `chat_file_read` returns text for: the structured text
/// kinds, SVG source, and a PDF, whose text is extracted under the same cap.
pub fn text_kind(mime: &str) -> bool {
    mime == "application/pdf"
        || mime == "image/svg+xml"
        || matches!(
            mime,
            "text/plain"
                | "text/markdown"
                | "text/csv"
                | "text/tab-separated-values"
                | "application/json"
                | "application/x-ndjson"
                | "application/yaml"
                | "application/xml"
        )
}

/// `true` for every kind that can be delivered to an image-capable
/// model as native image pixels (SVG through the bounded rasterizer).
pub fn visual_kind(mime: &str) -> bool {
    crate::attachment_image::visual_mime(mime)
}

/// One `chat_file_put_for_user` call: the scope it lands in, the owner it binds
/// to, and the caller's own delivery proof for the exact bytes about to be
/// published. Grouped so the retention call keeps a readable signature.
pub struct ChatFileUpload<'a, F> {
    pub scope: &'a str,
    pub context_id: &'a str,
    pub owner: (&'a str, &'a str),
    pub deliverable: F,
}

impl Store {
    /// Resolve a durable account for a server-verified principal. The principal
    /// is hashed before storage; display names and caller-supplied labels must
    /// never be passed here. Concurrent registration converges on one ID.
    pub fn chat_upload_account(&self, principal_hash: &str, authority: &str) -> Result<String> {
        if principal_hash.len() != 64
            || !principal_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || authority.is_empty()
            || authority.len() > 48
            || !authority
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'-')
        {
            return Err(Error::rejected("upload account principal is malformed"));
        }
        self.write_tx(|conn| {
            conn.execute(
                "INSERT INTO chat_upload_accounts(id, principal_hash, authority, created) \
                 VALUES(?,?,?,?) ON CONFLICT(principal_hash) DO NOTHING",
                params![
                    format!("acu-{}", uuid::Uuid::new_v4().simple()),
                    principal_hash,
                    authority,
                    now()
                ],
            )?;
            conn.query_row(
                "SELECT id FROM chat_upload_accounts WHERE principal_hash=?",
                [principal_hash],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })
    }

    /// Whether a canonical account ID names a durable registered principal.
    pub fn chat_upload_account_registered(&self, id: &str) -> Result<bool> {
        if id.len() != 36
            || !id.starts_with("acu-")
            || !id[4..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Ok(false);
        }
        let conn = self.conn();
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_upload_accounts WHERE id=?)",
            [id],
            |row| row.get(0),
        )?)
    }

    /// Look up an account already established by an authenticated native
    /// operation. Unlike registration this never creates authority.
    pub fn existing_chat_upload_account(&self, principal_hash: &str) -> Result<Option<String>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT id FROM chat_upload_accounts WHERE principal_hash=?",
            [principal_hash],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// The staged upload lands as a retained attachment. `tmp` must be
    /// a regular file directly under `<state>/wiki-uploads/` (the
    /// shared staging dir the board writes): the caller's parent is
    /// canonicalized so `..`/symlinked ancestors cannot escape, and the
    /// bytes are then read through the server-owned staging directory
    /// opened once (no-follow) with the leaf opened relative to that
    /// descriptor (no-follow, non-blocking) — a caller-controlled
    /// ancestor alias substituted after the check cannot redirect the
    /// read, and a symlink/FIFO/device leaf is refused. The
    /// store never deletes the caller's source `tmp` — it does not
    /// create it and cannot prove it owns it; the caller (the board
    /// route, or a direct RPC caller) owns that cleanup.
    ///
    /// The source is read with a bounded no-follow/nonblocking open
    /// (at most [`CHAT_FILE_MAX_BYTES`] + 1 bytes), the exact bytes
    /// read are written to an anonymous custody stage, and only those
    /// bytes are published — the checked bytes are the published bytes.
    /// The digest, kind, UTF-8 and secret checks run before publication.
    /// Retained bytes are workspace custody only; there is no state-dir
    /// fallback.
    ///
    /// `scope` is the stored provenance label the daemon derived, never
    /// a request field: `home` for the operator's own chat, or
    /// [`ChatFile::app_scope`]'s encoding of the verified installation
    /// and conversation with the proven context in `context_id`.
    /// Idempotent by (sha256, scope, context_id): same bytes in the same
    /// scope return the same row; different scope is a different row.
    pub fn chat_file_put_in_workspace(
        &self,
        roots: ChatFileStorageRoots<'_>,
        tmp: &Path,
        name: &str,
        scope: &str,
        context_id: &str,
        uploader: &str,
    ) -> Result<ChatFile> {
        let state_dir = roots.state_dir;
        let root = workspace_blob_dir(roots.workspace_dir, true)?
            .ok_or_else(|| Error::rejected("workspace custody directory is unavailable"))?;
        let name = sanitize_name(name)?;
        // The caller's path names a leaf under the staging dir and
        // proves (by canonicalized parent) that it claims that dir; the
        // bytes are then read through the server-owned staging
        // directory itself, never through the caller's alias, so a
        // substituted ancestor cannot redirect the read after the check.
        let Some(src) = open_upload_source(state_dir, tmp)? else {
            return Err(Error::rejected(format!(
                "chat_file_upload refused: the staged upload {} vanished",
                tmp.display()
            )));
        };
        let meta = src
            .metadata()
            .map_err(|e| Error::rejected(format!("chat_file_upload tmp {}: {e}", tmp.display())))?;
        if !meta.is_file() {
            return Err(Error::rejected(
                "chat_file_upload refused: tmp is not a regular file",
            ));
        }
        if meta.len() == 0 {
            return Err(Error::rejected(
                "chat_file_upload refused: the file is empty",
            ));
        }
        if meta.len() > CHAT_FILE_MAX_BYTES {
            return Err(Error::rejected(format!(
                "chat_file_upload refused: {} bytes over the {}-byte cap",
                meta.len(),
                CHAT_FILE_MAX_BYTES
            )));
        }

        // Serialize accounting, publication, and metadata commit across
        // processes. Acquire only after the confined source and its size
        // are validated, but before allocating the bounded upload buffer.
        // Replays remain protected by the same lock and do not allocate
        // another custody object.
        let _quota_lock = acquire_workspace_quota_lock(&root)?;

        // Read the source through the opened descriptor with a hard cap
        // — never an unbounded `fs::copy` — so a source growing after
        // the metadata check cannot consume unbounded disk. The bytes
        // read are the bytes checked and the bytes published.
        let bytes = read_bounded_file(&src, CHAT_FILE_MAX_BYTES)?;
        if bytes.len() as u64 != meta.len() {
            return Err(Error::rejected(
                "chat_file_upload refused: the staged upload changed while it was read",
            ));
        }
        let sha256 = sha256_hex(&bytes);
        let head = sniff_head(&bytes);
        let mime = check_kind(&name, head)?.to_string();
        // Every kind carries its read gate before retention: text/SVG is
        // extracted and scanned for credentials, a PDF through the
        // bounded converter, and a raster image decodes within the
        // delivery bound — the same check a read runs.
        checked_attachment_body(&name, &mime, &bytes)?;

        let existing = {
            let conn = self.conn();
            Self::chat_file_by_sha_in(&*conn, &sha256, scope, context_id)?
        };
        if let Some(existing) = existing {
            if let Some(stored) = open_blob_at(&root, &existing.sha256)? {
                let actual = read_bounded_file(&stored, CHAT_FILE_MAX_BYTES)?;
                if actual.len() as u64 != existing.size || sha256_hex(&actual) != existing.sha256 {
                    return Err(Error::rejected(
                        "existing workspace attachment bytes failed digest verification",
                    ));
                }
                return Ok(existing);
            }
        }
        enforce_workspace_quota(self, &root, roots.workspace_dir, scope, bytes.len() as u64)?;

        // Anonymous custody stage: write exactly the checked bytes and
        // re-read them bounded, so what is published is provably what
        // was checked. The stage has no name, so nothing is left behind
        // on any return path.
        let stage = write_stage_at(&root, &bytes)?;
        if stage.read_back(CHAT_FILE_MAX_BYTES)? != bytes {
            return Err(Error::rejected(
                "chat_file_upload refused: the custody stage did not match the checked bytes",
            ));
        }

        // Publish content-addressed, never overwriting and never
        // directly at the final name: a link of the stage's own inode is
        // atomic, and a racing writer that landed the same digest first
        // wins — the existing blob is then verified and a mismatch
        // refuses the upload rather than replacing it.
        publish_blob_at(&stage, &sha256, bytes.len() as u64)?;

        // The row: idempotent on (sha256, scope, context_id) — a retry
        // reads the row it already made.
        self.write_tx(|conn| {
            let tx = &mut *conn;
            if let Some(existing) = Self::chat_file_by_sha_in(tx, &sha256, scope, context_id)? {
                return Ok(existing);
            }
            let file = ChatFile {
                id: format!("chf-{}", uuid::Uuid::new_v4().simple()),
                sha256,
                size: bytes.len() as u64,
                mime,
                name,
                scope: scope.to_string(),
                context_id: context_id.to_string(),
                uploader: uploader.to_string(),
                user_id: None,
                thread_id: None,
                location_version: None,
                created: now(),
            };
            tx.execute(
                "INSERT INTO chat_files(id,sha256,size,mime,name,scope,context_id,uploader,created)
                 VALUES(?,?,?,?,?,?,?,?,?)",
                params![
                    file.id,
                    file.sha256,
                    file.size as i64,
                    file.mime,
                    file.name,
                    file.scope,
                    file.context_id,
                    file.uploader,
                    file.created
                ],
            )?;
            Ok(file)
        })
    }

    /// Retain a new original in the operational `users/<id>/threads/<id>/uploads`
    /// tree. The legacy PM root is used only for the shared quota lock; the
    /// anonymous custody stage and final hard link are on the destination
    /// operational filesystem, and ownership is versioned in SQLite.
    pub fn chat_file_put_for_user<F>(
        &self,
        roots: ChatFileOperationalRoots<'_>,
        tmp: &Path,
        name: &str,
        plan: ChatFileUpload<'_, F>,
    ) -> Result<ChatFile>
    where
        F: Fn(&str, &[u8]) -> Result<()>,
    {
        let ChatFileUpload {
            scope,
            context_id,
            owner,
            deliverable,
        } = plan;
        let (user_id, thread_id) = owner;
        if !valid_upload_account_id(user_id) || !valid_native_thread_id(thread_id) {
            return Err(Error::rejected(
                "chat upload requires a registered account and verified native conversation",
            ));
        }
        let legacy_root = workspace_blob_dir(roots.legacy_workspace_dir, true)?
            .ok_or_else(|| Error::rejected("legacy quota-lock directory is unavailable"))?;
        let name = sanitize_name(name)?;
        let Some(src) = open_upload_source(roots.state_dir, tmp)? else {
            return Err(Error::rejected("chat upload staging file is missing"));
        };
        let meta = src.metadata()?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > CHAT_FILE_MAX_BYTES {
            return Err(Error::rejected(
                "chat upload must be a nonempty regular file within the size cap",
            ));
        }
        let _quota_lock = acquire_workspace_quota_lock(&legacy_root)?;
        let bytes = read_bounded_file(&src, CHAT_FILE_MAX_BYTES)?;
        if bytes.len() as u64 != meta.len() {
            return Err(Error::rejected(
                "chat upload staging changed while it was read",
            ));
        }
        let sha256 = sha256_hex(&bytes);
        let mime = check_kind(&name, sniff_head(&bytes))?.to_string();
        checked_attachment_body(&name, &mime, &bytes)?;
        if let Some(existing) =
            self.chat_file_by_user_sha(&sha256, scope, context_id, user_id, thread_id)?
        {
            let actual = read_user_attachment(roots.workspace_dir, &existing)?;
            self.chat_file_ready_checked_bytes(&existing, actual)?;
            return Ok(existing);
        }
        // The caller's own delivery proof runs on the exact bytes about to be
        // published, before any custody or quota is committed: a visual kind
        // that cannot be screened, rendered or admitted by the selected model
        // is refused here, so nothing is retained, charged or reported ready.
        deliverable(&mime, &bytes)?;
        enforce_workspace_quota(
            self,
            &legacy_root,
            roots.workspace_dir,
            scope,
            bytes.len() as u64,
        )?;
        let id = format!("chf-{}", uuid::Uuid::new_v4().simple());
        let users = workspace_users_dir(roots.workspace_dir, true)?
            .ok_or_else(|| Error::rejected("operational users directory is unavailable"))?;
        let target = open_user_attachment_dir(&users, user_id, thread_id, &id, true)?
            .ok_or_else(|| Error::rejected("operational attachment directory is unavailable"))?;
        // Stage on the destination filesystem: immutable linkat publication
        // must not depend on the tracker and operational workspace sharing a
        // device. The legacy root remains only the shared quota lock/census.
        let stage = write_stage_at(&target, &bytes)?;
        if stage.read_back(CHAT_FILE_MAX_BYTES)? != bytes {
            return Err(Error::rejected(
                "chat upload custody stage changed before publication",
            ));
        }
        publish_attachment_at(&stage, &target, &name)?;
        self.write_tx(|conn| {
            if let Some(existing) = Self::chat_file_by_user_sha_in(
                conn, &sha256, scope, context_id, user_id, thread_id,
            )? {
                return Ok(existing);
            }
            let file = ChatFile {
                id,
                sha256,
                size: bytes.len() as u64,
                mime,
                name,
                scope: scope.to_string(),
                context_id: context_id.to_string(),
                uploader: user_id.to_string(),
                user_id: Some(user_id.to_string()),
                thread_id: Some(thread_id.to_string()),
                location_version: Some(1),
                created: now(),
            };
            conn.execute(
                "INSERT INTO chat_files(
                   id,sha256,size,mime,name,scope,context_id,uploader,created,
                   user_id,thread_id,location_version)
                 VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    file.id,
                    file.sha256,
                    file.size as i64,
                    file.mime,
                    file.name,
                    file.scope,
                    file.context_id,
                    file.uploader,
                    file.created,
                    file.user_id,
                    file.thread_id,
                    file.location_version,
                ],
            )?;
            Ok(file)
        })
    }

    fn chat_file_by_user_sha(
        &self,
        sha256: &str,
        scope: &str,
        context_id: &str,
        user_id: &str,
        thread_id: &str,
    ) -> Result<Option<ChatFile>> {
        let conn = self.conn();
        Self::chat_file_by_user_sha_in(&*conn, sha256, scope, context_id, user_id, thread_id)
    }

    fn chat_file_by_user_sha_in(
        tx: &impl super::StoreConn,
        sha256: &str,
        scope: &str,
        context_id: &str,
        user_id: &str,
        thread_id: &str,
    ) -> Result<Option<ChatFile>> {
        Ok(tx
            .query_row(
                "SELECT id,sha256,size,mime,name,scope,context_id,uploader,created,
                        user_id,thread_id,location_version
                 FROM chat_files WHERE sha256=? AND scope=? AND context_id=?
                   AND user_id=? AND thread_id=? AND location_version=1",
                params![sha256, scope, context_id, user_id, thread_id],
                row_chat_file,
            )
            .optional()?)
    }

    fn chat_file_by_sha_in(
        tx: &impl super::StoreConn,
        sha256: &str,
        scope: &str,
        context_id: &str,
    ) -> Result<Option<ChatFile>> {
        Ok(tx
            .query_row(
                "SELECT id,sha256,size,mime,name,scope,context_id,uploader,created,
                        user_id,thread_id,location_version
                 FROM chat_files WHERE sha256=? AND scope=? AND context_id=?
                   AND user_id IS NULL AND thread_id IS NULL",
                params![sha256, scope, context_id],
                row_chat_file,
            )
            .optional()?)
    }

    /// The retained row for `id`, `None` when unknown.
    pub fn chat_file(&self, id: &str) -> Result<Option<ChatFile>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "SELECT id,sha256,size,mime,name,scope,context_id,uploader,created,
                        user_id,thread_id,location_version
                 FROM chat_files WHERE id=?",
                [id],
                row_chat_file,
            )
            .optional()?)
    }

    /// The raw persisted app stamp of `message_id`'s enqueue entry —
    /// exactly what was stored at send time, without the live re-proof
    /// [`Store::message_app`] adds. The stamp is provenance, never
    /// authority: a caller that needs the binding to still hold must
    /// re-prove the context through [`Store::message_app`] or the
    /// context proof. Lives here, beside the scoped-file guard, because
    /// the scoped read needs to tell an installation-only binding
    /// (absent context) apart from a stale stamped context.
    pub fn message_app_stamp(&self, message_id: &str) -> Result<Option<Value>> {
        let conn = self.conn();
        Self::entry_app_in(&conn, message_id)
    }

    /// Workspace-rooted read of a row the caller already resolved and
    /// proved: the digest-verified bytes projected as bounded text. An
    /// existing symlink, FIFO, directory or corrupt blob refuses, and a
    /// missing blob refuses — there is no other custody to fall back to.
    pub fn chat_file_read_row_in_workspace(
        &self,
        workspace_dir: &Path,
        file: &ChatFile,
    ) -> Result<Value> {
        file.read_projection(read_workspace_blob(workspace_dir, file)?)
    }

    /// Read versioned custody without crossing between the legacy PM
    /// root and the operational account tree.
    pub fn chat_file_read_row_in_roots(
        &self,
        legacy_workspace: &Path,
        workspace: &Path,
        file: &ChatFile,
    ) -> Result<Value> {
        match file.location_version {
            Some(1) => {
                validate_operational_row(file)?;
                file.read_projection(read_user_attachment(workspace, file)?)
            }
            None | Some(0) => {
                validate_legacy_row(file)?;
                self.chat_file_read_row_in_workspace(legacy_workspace, file)
            }
            _ => Err(Error::rejected(
                "attachment location version is unsupported",
            )),
        }
    }

    /// Checked readiness for one retained row against the bytes on disk:
    /// the row must be an attachable kind within `cap` (clamped to
    /// [`CHAT_FILE_MAX_BYTES`]; a larger row refuses, never truncates),
    /// the workspace blob must exist as a regular file and its bounded
    /// content must still match the declared size and digest, sniff as
    /// its declared kind, pass the text/secret gate (or decode as a
    /// bounded raster image). Returns the exact checked bytes. This proves bytes only — scope,
    /// provenance and capability stay the caller's proof, and a caller
    /// holding an authorized operation must call this while that
    /// operation is still open.
    pub fn chat_file_ready_checked_in_workspace(
        &self,
        workspace_dir: &Path,
        file: &ChatFile,
        cap: u64,
    ) -> Result<Vec<u8>> {
        let cap = cap.min(CHAT_FILE_MAX_BYTES);
        if !text_kind(&file.mime) && !visual_kind(&file.mime) {
            return Err(Error::rejected(format!(
                "attachment '{}' is not a ready attachment — it is neither text/PDF \
                 nor a supported visual kind",
                file.id
            )));
        }
        if file.size > cap {
            return Err(Error::rejected(format!(
                "attachment '{}' is {} bytes — over the {cap}-byte read cap",
                file.id, file.size
            )));
        }
        let bytes = read_workspace_blob(workspace_dir, file)?;
        self.chat_file_ready_checked_bytes(file, bytes)
    }

    pub fn chat_file_ready_checked_in_roots(
        &self,
        legacy_workspace: &Path,
        workspace: &Path,
        file: &ChatFile,
        cap: u64,
    ) -> Result<Vec<u8>> {
        let cap = cap.min(CHAT_FILE_MAX_BYTES);
        if !text_kind(&file.mime) && !visual_kind(&file.mime) {
            return Err(Error::rejected(format!(
                "attachment '{}' is not a ready attachment — it is neither text/PDF \
                 nor a supported visual kind",
                file.id
            )));
        }
        if file.size > cap {
            return Err(Error::rejected(format!(
                "attachment '{}' is {} bytes — over the {cap}-byte read cap",
                file.id, file.size
            )));
        }
        match file.location_version {
            Some(1) => {
                validate_operational_row(file)?;
                self.chat_file_ready_checked_bytes(file, read_user_attachment(workspace, file)?)
            }
            None | Some(0) => {
                validate_legacy_row(file)?;
                self.chat_file_ready_checked_in_workspace(legacy_workspace, file, cap)
            }
            _ => Err(Error::rejected(
                "attachment location version is unsupported",
            )),
        }
    }

    fn chat_file_ready_checked_bytes(&self, file: &ChatFile, bytes: Vec<u8>) -> Result<Vec<u8>> {
        if bytes.is_empty() {
            return Err(Error::rejected(format!(
                "attachment '{}' is not ready — the retained file is empty",
                file.id
            )));
        }
        // The same kind proof the upload and every read run.
        prove_declared_kind(&file.name, &file.mime, &bytes)
            .map_err(|e| Error::rejected(format!("attachment '{}' is not ready — {e}", file.id)))?;
        // Readiness is the read gate: text/SVG must still extract and
        // pass the secret scan (a PDF through the bounded converter), and
        // a raster image must still decode within the delivery bound.
        checked_attachment_body(&file.name, &file.mime, &bytes)
            .map_err(|e| Error::rejected(format!("attachment '{}' is not ready — {e}", file.id)))?;
        Ok(bytes)
    }
}

fn row_chat_file(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatFile> {
    Ok(ChatFile {
        id: r.get(0)?,
        sha256: r.get(1)?,
        size: r.get::<_, i64>(2)? as u64,
        mime: r.get(3)?,
        name: r.get(4)?,
        scope: r.get(5)?,
        context_id: r.get(6)?,
        uploader: r.get(7)?,
        created: r.get(8)?,
        user_id: r.get(9)?,
        thread_id: r.get(10)?,
        location_version: r.get(11)?,
    })
}

/// sha256 of a byte slice, lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The digest grammar a stored row must carry: 64 lowercase hex chars.
fn valid_digest(sha256: &str) -> bool {
    sha256.len() == 64
        && sha256
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// A digest to name a custody entry — validated stored data, never
/// request data. A malformed digest refuses before any name is built.
fn validated_digest(sha256: &str) -> Result<&str> {
    if !valid_digest(sha256) {
        return Err(Error::rejected(
            "the retained row carries a malformed digest — refusing to derive a path from it",
        ));
    }
    Ok(sha256)
}

/// Open the workspace custody and read the row's blob, digest-verified.
/// A genuinely absent custody directory or blob refuses (the retained
/// bytes are missing); an aliased, non-regular or corrupt entry refuses.
fn validate_operational_row(file: &ChatFile) -> Result<()> {
    let user_id = file.user_id.as_deref().unwrap_or_default();
    let thread_id = file.thread_id.as_deref().unwrap_or_default();
    if !valid_upload_account_id(user_id)
        || !valid_native_thread_id(thread_id)
        || !valid_id(&file.id)
    {
        return Err(Error::rejected(
            "new-format attachment has malformed account, conversation or attachment ownership",
        ));
    }
    match ChatFile::parse_scope(&file.scope)? {
        None => file.home_scope()?,
        Some((_, conversation)) if conversation == thread_id => {}
        Some(_) => {
            return Err(Error::rejected(
                "new-format app attachment location does not match its conversation scope",
            ));
        }
    }
    Ok(())
}

fn validate_legacy_row(file: &ChatFile) -> Result<()> {
    if file.user_id.is_some() || file.thread_id.is_some() {
        return Err(Error::rejected(
            "legacy attachment unexpectedly carries new-format ownership metadata",
        ));
    }
    Ok(())
}

fn read_workspace_blob(workspace_dir: &Path, file: &ChatFile) -> Result<Vec<u8>> {
    let digest = validated_digest(&file.sha256)?;
    let root = workspace_blob_dir(workspace_dir, false)?;
    let bytes = match root {
        Some(root) => read_verified_at(&root, digest, file.size)?,
        None => None,
    };
    bytes.ok_or_else(|| {
        Error::rejected(format!(
            "attachment '{}' is not readable — the retained blob is missing",
            file.id
        ))
    })
}

/// A directory descriptor pinned after a no-follow component walk. Every
/// workspace blob operation is relative to this handle, never its pathname.
struct PinnedBlobDir {
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}

/// Open the workspace and custody suffix without following any component.
/// The suffix is created only on the upload path; reads are strictly
/// non-creating. `None` means the workspace exists but the custody suffix
/// is genuinely absent.
fn workspace_blob_dir(workspace: &Path, create: bool) -> Result<Option<PinnedBlobDir>> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        if !workspace.is_absolute() {
            return Err(Error::rejected("workspace custody path must be absolute"));
        }
        let root = std::ffi::CString::new("/").unwrap();
        let root_fd = unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if root_fd < 0 {
            return Err(Error::rejected(
                "cannot open filesystem root for workspace custody",
            ));
        }
        let mut current = unsafe { OwnedFd::from_raw_fd(root_fd) };
        let mut parts = Vec::new();
        for component in workspace.components() {
            match component {
                std::path::Component::Normal(name) => parts.push(name),
                std::path::Component::RootDir => {}
                _ => return Err(Error::rejected("workspace path is not normalized")),
            }
        }
        if parts.is_empty() {
            return Err(Error::rejected("workspace custody directory is invalid"));
        }
        for part in parts {
            let name = CString::new(part.as_bytes())
                .map_err(|_| Error::rejected("workspace path is invalid"))?;
            let fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(Error::rejected(
                    "workspace directory is missing, aliased, or unavailable",
                ));
            }
            current = unsafe { OwnedFd::from_raw_fd(fd) };
        }
        for child_name in [".cadence", CHAT_FILES_DIR] {
            let child = CString::new(child_name).unwrap();
            let mut fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    child.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ENOENT) && !create {
                    return Ok(None);
                }
                if error.raw_os_error() == Some(libc::ENOENT) && create {
                    if unsafe { libc::mkdirat(current.as_raw_fd(), child.as_ptr(), 0o700) } != 0
                        && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
                    {
                        return Err(Error::rejected(
                            "cannot create workspace chat custody directory",
                        ));
                    }
                    fd = unsafe {
                        libc::openat(
                            current.as_raw_fd(),
                            child.as_ptr(),
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                        )
                    };
                }
            }
            if fd < 0 {
                return Err(Error::rejected(
                    "workspace chat custody directory is aliased or unavailable",
                ));
            }
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(fd, &mut stat) } != 0
                || stat.st_uid != unsafe { libc::geteuid() }
                || stat.st_mode & 0o022 != 0
            {
                unsafe {
                    libc::close(fd);
                }
                return Err(Error::rejected(
                    "workspace custody directories must be owned by the daemon uid and not group/world writable",
                ));
            }
            current = unsafe { OwnedFd::from_raw_fd(fd) };
        }
        if create {
            ensure_custody_gitignore(current.as_raw_fd())?;
        }
        Ok(Some(PinnedBlobDir { fd: current }))
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, create);
        Err(Error::rejected(
            "descriptor-pinned workspace custody is unsupported on this platform",
        ))
    }
}

/// Open/create the operational users tree beneath the authoritative
/// workspace. Components are descriptor-walked without following symlinks;
/// the first-use ignore rule prevents user originals becoming tracked files.
fn workspace_users_dir(workspace: &Path, create: bool) -> Result<Option<PinnedBlobDir>> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        if !workspace.is_absolute() {
            return Err(Error::rejected("operational workspace must be absolute"));
        }
        let root_fd = unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if root_fd < 0 {
            return Err(Error::rejected(
                "cannot open filesystem root for operational workspace",
            ));
        }
        let mut current = unsafe { OwnedFd::from_raw_fd(root_fd) };
        for component in workspace.components() {
            let part = match component {
                std::path::Component::RootDir => continue,
                std::path::Component::Normal(part) => part,
                _ => return Err(Error::rejected("operational workspace is not normalized")),
            };
            let mut parent_stat: libc::stat = unsafe { std::mem::zeroed() };
            let daemon_uid = unsafe { libc::geteuid() };
            if unsafe { libc::fstat(current.as_raw_fd(), &mut parent_stat) } != 0
                || (parent_stat.st_uid != 0 && parent_stat.st_uid != daemon_uid)
                || (parent_stat.st_mode & 0o022 != 0 && parent_stat.st_mode & libc::S_ISVTX == 0)
            {
                return Err(Error::rejected(
                    "operational workspace parent has unsafe ownership or writable mode",
                ));
            }
            let name = CString::new(part.as_bytes())
                .map_err(|_| Error::rejected("operational workspace path is invalid"))?;
            let mut fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
                if !create {
                    return Ok(None);
                }
                if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) } != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
                {
                    return Err(Error::rejected("cannot create operational workspace root"));
                }
                fd = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
            }
            if fd < 0 {
                return Err(Error::rejected(
                    "operational workspace is aliased or unavailable",
                ));
            }
            current = unsafe { OwnedFd::from_raw_fd(fd) };
        }
        let mut workspace_stat: libc::stat = unsafe { std::mem::zeroed() };
        let daemon_uid = unsafe { libc::geteuid() };
        if unsafe { libc::fstat(current.as_raw_fd(), &mut workspace_stat) } != 0
            || workspace_stat.st_uid != daemon_uid
            || workspace_stat.st_mode & 0o022 != 0
        {
            return Err(Error::rejected(
                "operational workspace must be daemon-owned and not group/other writable",
            ));
        }
        let name = c"users";
        let mut fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            if !create {
                return Ok(None);
            }
            if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) } != 0
                && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
            {
                return Err(Error::rejected("cannot create operational users directory"));
            }
            fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
        }
        if fd < 0 {
            return Err(Error::rejected(
                "operational users directory is aliased or unavailable",
            ));
        }
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut stat) } != 0
            || stat.st_uid != unsafe { libc::geteuid() }
            || stat.st_mode & 0o077 != 0
        {
            unsafe { libc::close(fd) };
            return Err(Error::rejected(
                "operational users directory must be daemon-owned and private",
            ));
        }
        let users = unsafe { OwnedFd::from_raw_fd(fd) };
        if create {
            ensure_custody_gitignore(users.as_raw_fd())?;
        }
        Ok(Some(PinnedBlobDir { fd: users }))
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, create);
        Err(Error::rejected(
            "descriptor-pinned operational custody is unsupported on this platform",
        ))
    }
}

#[cfg(unix)]
fn open_user_attachment_dir(
    users: &PinnedBlobDir,
    user_id: &str,
    thread_id: &str,
    attachment_id: &str,
    create: bool,
) -> Result<Option<PinnedBlobDir>> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    if !valid_upload_account_id(user_id)
        || !valid_native_thread_id(thread_id)
        || !valid_id(attachment_id)
    {
        return Err(Error::rejected(
            "stored upload location fields are malformed",
        ));
    }
    let components = [user_id, "threads", thread_id, "uploads", attachment_id];
    let duplicate = unsafe { libc::fcntl(users.fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(Error::rejected(
            "cannot duplicate operational users directory",
        ));
    }
    let mut current = unsafe { OwnedFd::from_raw_fd(duplicate) };
    for part in components {
        let name = std::ffi::CString::new(part)
            .map_err(|_| Error::rejected("stored upload path is malformed"))?;
        let mut fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            if !create {
                return Ok(None);
            }
            if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) } != 0
                && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
            {
                return Err(Error::rejected("cannot create private upload directory"));
            }
            fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
        }
        if fd < 0 {
            return Err(Error::rejected(
                "private upload path is aliased or unavailable",
            ));
        }
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut stat) } != 0
            || stat.st_uid != unsafe { libc::geteuid() }
            || stat.st_mode & 0o077 != 0
        {
            unsafe { libc::close(fd) };
            return Err(Error::rejected(
                "private upload path has unsafe ownership or mode",
            ));
        }
        current = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    Ok(Some(PinnedBlobDir { fd: current }))
}

#[cfg(not(unix))]
fn open_user_attachment_dir(
    _users: &PinnedBlobDir,
    _user_id: &str,
    _thread_id: &str,
    _attachment_id: &str,
    _create: bool,
) -> Result<Option<PinnedBlobDir>> {
    Err(Error::rejected(
        "descriptor-pinned upload custody is unsupported",
    ))
}

#[cfg(target_os = "linux")]
fn publish_attachment_at(stage: &PinnedStage, target: &PinnedBlobDir, name: &str) -> Result<()> {
    use std::os::fd::AsRawFd;
    if sanitize_name(name)? != name {
        return Err(Error::rejected("host-validated upload filename changed"));
    }
    let name = std::ffi::CString::new(name)
        .map_err(|_| Error::rejected("host-validated upload filename is invalid"))?;
    let mut rc = unsafe {
        libc::linkat(
            stage.file.as_raw_fd(),
            c"".as_ptr(),
            target.fd.as_raw_fd(),
            name.as_ptr(),
            libc::AT_EMPTY_PATH,
        )
    };
    if rc != 0
        && matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM) | Some(libc::EINVAL)
        )
    {
        let procfd = std::ffi::CString::new(format!("/proc/self/fd/{}", stage.file.as_raw_fd()))
            .expect("formatted procfd contains no NUL");
        rc = unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                procfd.as_ptr(),
                target.fd.as_raw_fd(),
                name.as_ptr(),
                libc::AT_SYMLINK_FOLLOW,
            )
        };
    }
    if rc != 0 {
        return Err(Error::rejected(format!(
            "cannot publish immutable upload without replacing an existing file: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn publish_attachment_at(_stage: &PinnedStage, _target: &PinnedBlobDir, _name: &str) -> Result<()> {
    Err(Error::rejected(
        "anonymous immutable upload publication is unsupported",
    ))
}

#[cfg(unix)]
fn read_user_attachment(workspace: &Path, file: &ChatFile) -> Result<Vec<u8>> {
    let user_id = file
        .user_id
        .as_deref()
        .ok_or_else(|| Error::rejected("new-format attachment has no account owner"))?;
    let thread_id = file
        .thread_id
        .as_deref()
        .ok_or_else(|| Error::rejected("new-format attachment has no conversation binding"))?;
    let users = workspace_users_dir(workspace, false)?
        .ok_or_else(|| Error::rejected("operational users directory is missing"))?;
    let dir = open_user_attachment_dir(&users, user_id, thread_id, &file.id, false)?
        .ok_or_else(|| Error::rejected("operational attachment directory is missing"))?;
    if sanitize_name(&file.name)? != file.name {
        return Err(Error::rejected("stored upload filename is malformed"));
    }
    let name = std::ffi::CString::new(file.name.as_str())
        .map_err(|_| Error::rejected("stored upload filename is malformed"))?;
    let fd = unsafe {
        libc::openat(
            dir.fd.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(Error::rejected(
            "operational attachment is missing or aliased",
        ));
    }
    let opened = unsafe { std::fs::File::from_raw_fd(fd) };
    let bytes = read_bounded_file(&opened, CHAT_FILE_MAX_BYTES)?;
    if bytes.is_empty() || bytes.len() as u64 != file.size || sha256_hex(&bytes) != file.sha256 {
        return Err(Error::rejected(
            "operational attachment failed size or digest verification",
        ));
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_user_attachment(_workspace: &Path, _file: &ChatFile) -> Result<Vec<u8>> {
    Err(Error::rejected(
        "descriptor-pinned upload custody is unsupported",
    ))
}

/// The workspace is the tracker's git working tree: keep custody bytes out
/// of it with a `*` ignore rule inside the custody directory, written
/// through the pinned descriptor (never following a symlink). A missing
/// rule is created exclusively; an existing entry that is not exactly
/// `*` is replaced by an atomic rename of a fresh file in the same dir.
#[cfg(unix)]
fn ensure_custody_gitignore(dir: std::os::fd::RawFd) -> Result<()> {
    use std::io::{Read, Write};
    use std::os::fd::{FromRawFd, OwnedFd};
    let err =
        |e: std::io::Error| Error::rejected(format!("cannot write the custody ignore rule: {e}"));
    let open = |name: &std::ffi::CStr, flags: libc::c_int| unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    // True when `.gitignore` is already exactly the `*` rule.
    let has_rule = || {
        let fd = open(c".gitignore", libc::O_RDONLY | libc::O_NONBLOCK);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let mut buf = Vec::new();
        // Bounded: the rule is two bytes; anything longer is rewritten.
        let ok = Read::take(&mut file, 16).read_to_end(&mut buf).is_ok();
        Ok(ok && (buf == b"*\n" || buf == b"*"))
    };
    match has_rule() {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        // ELOOP/other: a symlink or odd entry is replaced below by rename,
        // which never follows it.
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ELOOP)) => {}
        Err(e) => return Err(err(e)),
    }
    // A unique temp name per call: concurrent first uploads must not
    // unlink or rename each other's file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = std::ffi::CString::new(format!(
        ".gitignore.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
    .expect("no NUL in a formatted name");
    let written = (|| {
        let fd = open(&tmp, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        file.write_all(b"*\n")?;
        file.sync_all()?;
        if unsafe { libc::renameat(dir, tmp.as_ptr(), dir, c".gitignore".as_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    })();
    if let Err(e) = written {
        unsafe { libc::unlinkat(dir, tmp.as_ptr(), 0) };
        // A concurrent first upload may have installed the rule already.
        return if has_rule().unwrap_or(false) {
            Ok(())
        } else {
            Err(err(e))
        };
    }
    Ok(())
}

#[cfg(unix)]
fn open_blob_at(root: &PinnedBlobDir, digest: &str) -> Result<Option<std::fs::File>> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let name =
        std::ffi::CString::new(digest).map_err(|_| Error::rejected("invalid retained digest"))?;
    let fd = unsafe {
        libc::openat(
            root.fd.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(Error::rejected(format!(
            "workspace retained entry is invalid: {e}"
        )));
    }
    Ok(Some(std::fs::File::from(unsafe {
        OwnedFd::from_raw_fd(fd)
    })))
}

#[cfg(unix)]
fn read_verified_at(root: &PinnedBlobDir, digest: &str, size: u64) -> Result<Option<Vec<u8>>> {
    let Some(file) = open_blob_at(root, digest)? else {
        return Ok(None);
    };
    let bytes = read_bounded_file(&file, CHAT_FILE_MAX_BYTES)?;
    if bytes.len() as u64 != size || sha256_hex(&bytes) != digest {
        return Err(Error::rejected(
            "the retained bytes failed their digest check — they no longer match the row",
        ));
    }
    Ok(Some(bytes))
}

#[cfg(not(unix))]
fn read_verified_at(_root: &PinnedBlobDir, _digest: &str, _size: u64) -> Result<Option<Vec<u8>>> {
    Err(Error::rejected(
        "descriptor-pinned workspace custody is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn acquire_workspace_quota_lock(root: &PinnedBlobDir) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let name = c".quota.lock";
    let mut fd = unsafe {
        libc::openat(
            root.fd.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) {
        fd = unsafe {
            libc::openat(
                root.fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
    }
    if fd < 0 {
        return Err(Error::rejected(format!(
            "workspace quota lock unavailable: {}",
            std::io::Error::last_os_error()
        )));
    }
    let lock = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(lock.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(Error::rejected(
            "workspace quota lock could not be verified",
        ));
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_uid != unsafe { libc::getuid() }
        || stat.st_nlink != 1
        || stat.st_mode & 0o777 != 0o600
    {
        return Err(Error::rejected(
            "workspace quota lock has unsafe ownership or mode",
        ));
    }
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::busy(
            "workspace quota admission is busy; retry shortly",
        ));
    }
    Ok(lock)
}

#[cfg(not(unix))]
fn acquire_workspace_quota_lock(_root: &PinnedBlobDir) -> Result<()> {
    Err(Error::rejected(
        "workspace quota locking is unsupported on this platform",
    ))
}

#[cfg(unix)]
struct QuotaFile {
    path: Vec<u8>,
    device: libc::dev_t,
    inode: libc::ino_t,
    size: u64,
}

/// Reset the thread's `errno` so a `readdir` end-of-directory can be told
/// from a read error.
#[cfg(unix)]
fn clear_errno() {
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(target_os = "macos")]
    unsafe {
        *libc::__error() = 0;
    }
}

#[cfg(unix)]
struct QuotaDir(*mut libc::DIR);

#[cfg(unix)]
impl Drop for QuotaDir {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0);
        }
    }
}

#[cfg(unix)]
fn quota_scan_dir(fd: std::os::fd::RawFd) -> Result<Vec<QuotaFile>> {
    let scan_fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if scan_fd < 0 {
        return Err(Error::rejected(
            "quota accounting could not duplicate a pinned directory",
        ));
    }
    let dir = unsafe { libc::fdopendir(scan_fd) };
    if dir.is_null() {
        unsafe { libc::close(scan_fd) };
        return Err(Error::rejected(
            "quota accounting could not enumerate a pinned directory",
        ));
    }
    let dir = QuotaDir(dir);
    let mut files = Vec::new();
    let mut entries = 0u64;
    loop {
        clear_errno();
        let entry = unsafe { libc::readdir(dir.0) };
        if entry.is_null() {
            let read_error = std::io::Error::last_os_error();
            if read_error.raw_os_error().is_some_and(|code| code != 0) {
                return Err(Error::rejected(
                    "quota accounting directory scan was incomplete",
                ));
            }
            break;
        }
        let raw = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        if raw.to_bytes() == b"." || raw.to_bytes() == b".." {
            continue;
        }
        if matches!(raw.to_bytes(), b".quota.lock" | b".gitignore") {
            continue;
        }
        entries = entries
            .checked_add(1)
            .ok_or_else(|| Error::rejected("quota entry count overflow"))?;
        if entries > 1000 {
            return Err(Error::rejected(
                "quota accounting refuses more than 1,000 entries in a residue directory",
            ));
        }
        let name = raw.to_owned();
        let child = unsafe {
            libc::openat(
                fd,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if child < 0 {
            return Err(Error::rejected(
                "quota accounting found an unreadable or aliased entry",
            ));
        }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let rc = unsafe { libc::fstat(child, stat.as_mut_ptr()) };
        if rc != 0 {
            unsafe {
                libc::close(child);
            }
            return Err(Error::rejected(
                "quota accounting could not inspect an entry",
            ));
        }
        let stat = unsafe { stat.assume_init() };
        unsafe {
            libc::close(child);
        }
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_size < 0 {
            return Err(Error::rejected(
                "quota accounting refuses non-regular or ambiguous entries",
            ));
        }
        files.push(QuotaFile {
            path: raw.to_bytes().to_vec(),
            device: stat.st_dev,
            inode: stat.st_ino,
            size: stat.st_size as u64,
        });
    }
    Ok(files)
}

#[cfg(unix)]
fn quota_scan_user_tree(root: &PinnedBlobDir) -> Result<Vec<QuotaFile>> {
    use std::os::fd::AsRawFd;
    fn walk(
        fd: std::os::fd::RawFd,
        prefix: &[u8],
        depth: usize,
        entries: &mut u64,
        files: &mut Vec<QuotaFile>,
    ) -> Result<()> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        if depth > 8 {
            return Err(Error::rejected(
                "quota accounting found excessive upload-directory depth",
            ));
        }
        let scan_fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if scan_fd < 0 {
            return Err(Error::rejected(
                "quota accounting could not duplicate a users directory",
            ));
        }
        let dir = unsafe { libc::fdopendir(scan_fd) };
        if dir.is_null() {
            unsafe { libc::close(scan_fd) };
            return Err(Error::rejected(
                "quota accounting could not enumerate a users directory",
            ));
        }
        let dir = QuotaDir(dir);
        loop {
            clear_errno();
            let entry = unsafe { libc::readdir(dir.0) };
            if entry.is_null() {
                if std::io::Error::last_os_error()
                    .raw_os_error()
                    .is_some_and(|code| code != 0)
                {
                    return Err(Error::rejected(
                        "quota accounting users scan was incomplete",
                    ));
                }
                break;
            }
            let raw = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
            if matches!(raw.to_bytes(), b"." | b"..")
                || (depth == 0 && raw.to_bytes() == b".gitignore")
            {
                continue;
            }
            *entries = entries
                .checked_add(1)
                .ok_or_else(|| Error::rejected("quota entry count overflow"))?;
            if *entries > 10_000 {
                return Err(Error::rejected(
                    "quota accounting refuses an excessive users tree",
                ));
            }
            let name = raw.to_owned();
            let child = unsafe {
                libc::openat(
                    fd,
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if child < 0 {
                return Err(Error::rejected(
                    "quota accounting found an unreadable or aliased upload entry",
                ));
            }
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe { libc::fstat(child, stat.as_mut_ptr()) } != 0 {
                unsafe { libc::close(child) };
                return Err(Error::rejected(
                    "quota accounting could not inspect an upload entry",
                ));
            }
            let stat = unsafe { stat.assume_init() };
            match stat.st_mode & libc::S_IFMT {
                libc::S_IFDIR => {
                    let mut nested = prefix.to_vec();
                    nested.push(b'/');
                    nested.extend_from_slice(raw.to_bytes());
                    let nested_fd = unsafe { OwnedFd::from_raw_fd(child) };
                    walk(nested_fd.as_raw_fd(), &nested, depth + 1, entries, files)?;
                }
                libc::S_IFREG if stat.st_size >= 0 => {
                    unsafe { libc::close(child) };
                    let mut path = prefix.to_vec();
                    path.push(b'/');
                    path.extend_from_slice(raw.to_bytes());
                    files.push(QuotaFile {
                        path,
                        device: stat.st_dev,
                        inode: stat.st_ino,
                        size: stat.st_size as u64,
                    });
                }
                _ => {
                    unsafe { libc::close(child) };
                    return Err(Error::rejected(
                        "quota accounting refuses non-regular or ambiguous upload entries",
                    ));
                }
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(root.fd.as_raw_fd(), b"users", 0, &mut 0, &mut files)?;
    Ok(files)
}

#[cfg(not(unix))]
fn quota_scan_user_tree(_root: &PinnedBlobDir) -> Result<Vec<QuotaFile>> {
    Err(Error::rejected(
        "quota accounting is unsupported on this platform",
    ))
}

/// The stable code a quota refusal carries (the board maps it to 413).
pub const CHAT_QUOTA_CODE: &str = "chat_quota_reached";

fn quota_reached(which: &str) -> Error {
    Error::invalid(
        CHAT_QUOTA_CODE,
        format!("chat attachment {which} quota reached"),
    )
}

struct QuotaMetadataLedger {
    known_blobs: std::collections::HashSet<(Vec<u8>, u64)>,
    known_user_paths: std::collections::HashSet<(Vec<u8>, u64)>,
    bucket_bytes: u64,
    bucket_objects: u64,
    metadata_objects: u64,
}

fn quota_metadata_ledger(store: &Store, scope: &str) -> Result<QuotaMetadataLedger> {
    const INSTANCE_OBJECTS: u64 = 1000;
    let bucket_install = if scope == CHAT_FILE_SCOPE_HOME {
        None
    } else {
        Some(
            ChatFile::parse_scope(scope)?
                .ok_or_else(|| Error::rejected("invalid quota scope"))?
                .0
                .to_owned(),
        )
    };
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT id,sha256,size,mime,name,scope,context_id,uploader,created,
                user_id,thread_id,location_version FROM chat_files",
    )?;
    let mut rows = stmt.query([])?;
    let mut ledger = QuotaMetadataLedger {
        known_blobs: std::collections::HashSet::new(),
        known_user_paths: std::collections::HashSet::new(),
        bucket_bytes: 0,
        bucket_objects: 0,
        metadata_objects: 0,
    };
    while let Some(row) = rows.next()? {
        let file = row_chat_file(row)?;
        if file.size > CHAT_FILE_MAX_BYTES || !valid_digest(&file.sha256) {
            return Err(Error::rejected(
                "quota accounting found invalid original metadata",
            ));
        }
        let parsed_scope = ChatFile::parse_scope(&file.scope)?;
        let same_bucket = match (bucket_install.as_deref(), parsed_scope) {
            (None, None) => true,
            (Some(wanted), Some((install, _))) => install == wanted,
            _ => false,
        };
        ledger.metadata_objects = ledger
            .metadata_objects
            .checked_add(1)
            .ok_or_else(|| Error::rejected("quota object count overflow"))?;
        if ledger.metadata_objects > INSTANCE_OBJECTS {
            return Err(quota_reached("instance"));
        }
        match (
            file.location_version,
            file.user_id.as_deref(),
            file.thread_id.as_deref(),
        ) {
            (None, None, None) => {
                ledger
                    .known_blobs
                    .insert((file.sha256.as_bytes().to_vec(), file.size));
            }
            (Some(1), Some(user), Some(thread))
                if valid_upload_account_id(user)
                    && valid_native_thread_id(thread)
                    && valid_id(&file.id)
                    && sanitize_name(&file.name).ok().as_deref() == Some(file.name.as_str()) =>
            {
                let path = format!(
                    "users/{user}/threads/{thread}/uploads/{}/{}",
                    file.id, file.name
                );
                ledger
                    .known_user_paths
                    .insert((path.into_bytes(), file.size));
            }
            _ => {
                return Err(Error::rejected(
                    "quota accounting found malformed versioned ownership metadata",
                ));
            }
        }
        if same_bucket {
            ledger.bucket_bytes = ledger
                .bucket_bytes
                .checked_add(file.size)
                .ok_or_else(|| Error::rejected("quota byte count overflow"))?;
            ledger.bucket_objects = ledger
                .bucket_objects
                .checked_add(1)
                .ok_or_else(|| Error::rejected("quota object count overflow"))?;
        }
    }
    Ok(ledger)
}

fn enforce_workspace_quota(
    store: &Store,
    root: &PinnedBlobDir,
    workspace_dir: &Path,
    scope: &str,
    new_size: u64,
) -> Result<()> {
    const INSTANCE_BYTES: u64 = 1024 * 1024 * 1024;
    const INSTANCE_OBJECTS: u64 = 1000;
    const BUCKET_BYTES: u64 = 256 * 1024 * 1024;
    const BUCKET_OBJECTS: u64 = 250;
    #[cfg(unix)]
    {
        let mut observed = quota_scan_dir(root.fd.as_raw_fd())?;
        if let Some(users) = workspace_users_dir(workspace_dir, false)? {
            observed.extend(quota_scan_user_tree(&users)?);
        }
        let ledger = quota_metadata_ledger(store, scope)?;
        let known_blobs = ledger.known_blobs;
        let known_user_paths = ledger.known_user_paths;
        let bucket_bytes = ledger.bucket_bytes;
        let bucket_metadata = ledger.bucket_objects;
        let metadata_objects = ledger.metadata_objects;
        let mut physical =
            std::collections::HashMap::<(libc::dev_t, libc::ino_t), (u64, bool)>::new();
        for file in observed {
            let known = if file.path.starts_with(b"users/") {
                known_user_paths.contains(&(file.path, file.size))
            } else {
                known_blobs.contains(&(file.path, file.size))
            };
            let entry = physical
                .entry((file.device, file.inode))
                .or_insert((file.size, false));
            if entry.0 != file.size {
                return Err(Error::rejected(
                    "quota accounting observed inconsistent inode sizes",
                ));
            }
            entry.1 |= !known;
        }
        let mut physical_bytes = 0u64;
        let mut unknown_bytes = 0u64;
        let mut unknown_objects = 0u64;
        for (size, unknown) in physical.values().copied() {
            physical_bytes = physical_bytes
                .checked_add(size)
                .ok_or_else(|| Error::rejected("quota byte count overflow"))?;
            if unknown {
                unknown_bytes = unknown_bytes
                    .checked_add(size)
                    .ok_or_else(|| Error::rejected("quota byte count overflow"))?;
                unknown_objects = unknown_objects
                    .checked_add(1)
                    .ok_or_else(|| Error::rejected("quota object count overflow"))?;
            }
        }
        let physical_files = u64::try_from(physical.len())
            .map_err(|_| Error::rejected("quota object count overflow"))?;
        let projected_instance = physical_bytes
            .checked_add(new_size)
            .ok_or_else(|| Error::rejected("quota byte count overflow"))?;
        // Reserve one prospective physical custody file and one metadata row.
        // An exact scoped replay returns before this admission check.
        let projected_objects = physical_files
            .checked_add(metadata_objects)
            .and_then(|n| n.checked_add(2))
            .ok_or_else(|| Error::rejected("quota object count overflow"))?;
        let projected_bucket_bytes = bucket_bytes
            .checked_add(unknown_bytes)
            .and_then(|n| n.checked_add(new_size))
            .ok_or_else(|| Error::rejected("quota byte count overflow"))?;
        let projected_bucket_objects = bucket_metadata
            .checked_add(unknown_objects)
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| Error::rejected("quota object count overflow"))?;
        if projected_instance > INSTANCE_BYTES || projected_objects > INSTANCE_OBJECTS {
            return Err(quota_reached("instance"));
        }
        if projected_bucket_bytes > BUCKET_BYTES || projected_bucket_objects > BUCKET_OBJECTS {
            return Err(quota_reached("scope"));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (
            store,
            root,
            scope,
            new_size,
            INSTANCE_BYTES,
            INSTANCE_OBJECTS,
            BUCKET_BYTES,
            BUCKET_OBJECTS,
        );
        Err(Error::rejected(
            "workspace quota accounting is unsupported on this platform",
        ))
    }
}

/// Read an already-opened descriptor with a hard cap. The descriptor is
/// verified to be a regular file and its length checked BEFORE any
/// read, so a declared row can never cause an arbitrarily large read,
/// and the bytes returned are exactly the bytes whose length was
/// checked. The caller already confined the open (no-follow,
/// non-blocking); nothing is re-opened by path.
fn read_bounded_file(f: &std::fs::File, cap: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(Error::rejected(
            "not a regular file — refusing symlinks, FIFOs and devices",
        ));
    }
    if meta.len() > cap {
        return Err(Error::rejected(format!(
            "the bytes grew past the {cap}-byte cap on disk"
        )));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    f.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != meta.len() {
        return Err(Error::rejected(
            "the bytes changed while they were being read",
        ));
    }
    Ok(bytes)
}

/// Open the caller-supplied upload source confined to the server-owned
/// staging directory. The caller's path is used only to name a leaf and
/// to prove (by canonicalized parent) that it claims
/// `<state>/wiki-uploads`; the read itself opens that server-owned
/// directory by its lexical path once (no-follow directory descriptor —
/// a symlinked staging dir refuses) and opens the leaf relative to it
/// with `O_NOFOLLOW|O_NONBLOCK`, so a caller-controlled ancestor alias
/// substituted after the check cannot redirect the read and a
/// symlink/FIFO/device leaf is refused without blocking.
/// `Ok(None)` when the leaf does not exist.
fn open_upload_source(state_dir: &Path, tmp: &Path) -> Result<Option<std::fs::File>> {
    let uploads = state_dir.join(crate::wiki::UPLOAD_DIR);
    let uploads_canon = uploads.canonicalize().unwrap_or_else(|_| uploads.clone());
    let parent = tmp.parent().ok_or_else(|| {
        Error::rejected(format!(
            "chat_file_upload refused: tmp {} has no parent directory",
            tmp.display()
        ))
    })?;
    let parent_canon = parent
        .canonicalize()
        .map_err(|e| Error::rejected(format!("chat_file_upload tmp {}: {e}", tmp.display())))?;
    if parent_canon != uploads_canon {
        return Err(Error::rejected(format!(
            "chat_file_upload refused: tmp must be a file directly under {}",
            uploads.display()
        )));
    }
    let name = tmp.file_name().ok_or_else(|| {
        Error::rejected(format!(
            "chat_file_upload refused: tmp {} has no file name",
            tmp.display()
        ))
    })?;
    if name.is_empty() || name == "." || name == ".." {
        return Err(Error::rejected(format!(
            "chat_file_upload refused: tmp {} is not a normal leaf name",
            tmp.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        let dir_c = std::ffi::CString::new(uploads.as_os_str().as_bytes())
            .map_err(|_| Error::rejected("chat_file_upload refused: staging path is not usable"))?;
        let dirfd = unsafe {
            libc::open(
                dir_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if dirfd < 0 {
            let e = std::io::Error::last_os_error();
            // `O_DIRECTORY|O_NOFOLLOW` on a symlink is `ENOTDIR` on
            // Linux, not `ELOOP`: name the refusal from lstat.
            if e.raw_os_error() == Some(libc::ELOOP)
                || std::fs::symlink_metadata(&uploads)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false)
            {
                return Err(Error::rejected(format!(
                    "chat_file_upload refused: the staging dir {} is a symlink — refusing to follow it",
                    uploads.display()
                )));
            }
            return Err(Error::rejected(format!(
                "chat_file_upload refused: cannot open the staging dir {}: {e}",
                uploads.display()
            )));
        }
        let dirfd = unsafe { OwnedFd::from_raw_fd(dirfd) };
        let leaf = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| Error::rejected("chat_file_upload refused: tmp name is not usable"))?;
        let fd = unsafe {
            libc::openat(
                dirfd.as_raw_fd(),
                leaf.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ENOENT) {
                return Ok(None);
            }
            if e.raw_os_error() == Some(libc::ELOOP) {
                return Err(Error::rejected(
                    "chat_file_upload refused: tmp is a symlink — refusing to follow it",
                ));
            }
            return Err(Error::rejected(format!(
                "chat_file_upload tmp {}: {e}",
                tmp.display()
            )));
        }
        Ok(Some(std::fs::File::from(unsafe {
            OwnedFd::from_raw_fd(fd)
        })))
    }
    #[cfg(not(unix))]
    {
        match std::fs::File::open(uploads_canon.join(name)) {
            Ok(f) => Ok(Some(f)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::rejected(format!(
                "chat_file_upload tmp {}: {e}",
                tmp.display()
            ))),
        }
    }
}

/// An anonymous (`O_TMPFILE`) custody inode in the pinned directory.
/// Publication links this very inode, so the published bytes are the
/// checked bytes and no named stage can be raced or left behind.
#[cfg(target_os = "linux")]
struct PinnedStage {
    root_fd: std::os::fd::OwnedFd,
    file: std::fs::File,
}

#[cfg(target_os = "linux")]
impl PinnedStage {
    fn read_back(&self, cap: u64) -> Result<Vec<u8>> {
        use std::io::Seek;
        let mut file = self.file.try_clone()?;
        file.seek(std::io::SeekFrom::Start(0))?;
        read_bounded_file(&file, cap)
    }
}

#[cfg(target_os = "linux")]
fn write_stage_at(root: &PinnedBlobDir, bytes: &[u8]) -> Result<PinnedStage> {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let fd = unsafe {
        libc::openat(
            root.fd.as_raw_fd(),
            c".".as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(Error::rejected(format!(
            "workspace filesystem does not support anonymous O_TMPFILE custody; refusing named-stage fallback: {}",
            std::io::Error::last_os_error()
        )));
    }
    let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    if !file.metadata()?.is_file() {
        return Err(Error::rejected(
            "anonymous workspace stage is not a regular file",
        ));
    }
    let stage = PinnedStage {
        root_fd: duplicate_fd(root.fd.as_raw_fd())?,
        file,
    };
    let mut writer = stage.file.try_clone()?;
    writer.write_all(bytes)?;
    writer.sync_all()?;
    let mut check = stage.file.try_clone()?;
    use std::io::Seek;
    check.seek(std::io::SeekFrom::Start(0))?;
    let reread = read_bounded_file(&check, CHAT_FILE_MAX_BYTES)?;
    if reread != bytes {
        return Err(Error::rejected(
            "workspace custody stage changed before publication",
        ));
    }
    Ok(stage)
}

/// Anonymous `O_TMPFILE` custody is Linux-only; elsewhere an upload
/// fails closed rather than falling back to a named, racy stage.
#[cfg(not(target_os = "linux"))]
enum PinnedStage {}

#[cfg(not(target_os = "linux"))]
impl PinnedStage {
    fn read_back(&self, _cap: u64) -> Result<Vec<u8>> {
        match *self {}
    }
}

#[cfg(not(target_os = "linux"))]
fn write_stage_at(_root: &PinnedBlobDir, _bytes: &[u8]) -> Result<PinnedStage> {
    Err(Error::rejected(
        "chat attachments are not supported on this platform",
    ))
}

#[cfg(not(target_os = "linux"))]
fn publish_blob_at(stage: &PinnedStage, _digest: &str, _size: u64) -> Result<()> {
    match *stage {}
}

#[cfg(target_os = "linux")]
fn publish_blob_at(stage: &PinnedStage, digest: &str, size: u64) -> Result<()> {
    use std::os::fd::AsRawFd;
    let dest = std::ffi::CString::new(digest).unwrap();
    let mut rc = unsafe {
        libc::linkat(
            stage.file.as_raw_fd(),
            c"".as_ptr(),
            stage.root_fd.as_raw_fd(),
            dest.as_ptr(),
            libc::AT_EMPTY_PATH,
        )
    };
    let mut error = std::io::Error::last_os_error();
    if rc != 0 && matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EINVAL)) {
        // AT_EMPTY_PATH can require CAP_DAC_READ_SEARCH. The procfd link
        // still names the opened inode, never a re-resolved stage leaf.
        let procfd =
            std::ffi::CString::new(format!("/proc/self/fd/{}", stage.file.as_raw_fd())).unwrap();
        rc = unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                procfd.as_ptr(),
                stage.root_fd.as_raw_fd(),
                dest.as_ptr(),
                libc::AT_SYMLINK_FOLLOW,
            )
        };
        error = std::io::Error::last_os_error();
    }
    if rc == 0 {
        return Ok(());
    }
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        if matches!(
            read_verified_at(
                &PinnedBlobDir {
                    fd: duplicate_fd(stage.root_fd.as_raw_fd())?,
                },
                digest,
                size
            ),
            Ok(Some(_))
        ) {
            return Ok(());
        }
        return Err(Error::rejected(
            "existing workspace blob failed digest verification; refusing overwrite",
        ));
    }
    Err(Error::rejected(format!(
        "could not publish workspace blob from its owned inode: {error}"
    )))
}

#[cfg(target_os = "linux")]
fn duplicate_fd(fd: std::os::fd::RawFd) -> Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if copy < 0 {
        return Err(Error::rejected(
            "cannot retain workspace custody descriptor",
        ));
    }
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(copy) })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
        (dir, s)
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_first_gitignore_writes_all_succeed() {
        use std::os::fd::AsRawFd;
        let dir = TempDir::new().unwrap();
        let handle = std::fs::File::open(dir.path()).unwrap();
        let fd = handle.as_raw_fd();
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| ensure_custody_gitignore(fd)))
                .collect();
            for worker in workers {
                worker.join().unwrap().unwrap();
            }
        });
        assert_eq!(
            std::fs::read(dir.path().join(".gitignore")).unwrap(),
            b"*\n"
        );
    }

    fn stage(state_dir: &Path, bytes: &[u8]) -> PathBuf {
        let dir = state_dir.join(crate::wiki::UPLOAD_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(format!("upload-{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&tmp, bytes).unwrap();
        tmp
    }

    fn workspace(state_dir: &Path) -> PathBuf {
        let ws = state_dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        ws
    }

    fn put_as(s: &Store, dir: &Path, tmp: &Path, name: &str, scope: &str) -> Result<ChatFile> {
        s.chat_file_put_in_workspace(
            ChatFileStorageRoots {
                state_dir: dir,
                workspace_dir: &workspace(dir),
            },
            tmp,
            name,
            scope,
            "",
            "operator",
        )
    }

    fn put(s: &Store, dir: &Path, bytes: &[u8], name: &str) -> Result<ChatFile> {
        put_as(s, dir, &stage(dir, bytes), name, CHAT_FILE_SCOPE_HOME)
    }

    fn read(s: &Store, dir: &Path, file: &ChatFile) -> Result<Value> {
        s.chat_file_read_row_in_workspace(&workspace(dir), file)
    }

    #[test]
    fn put_retains_text_and_read_returns_it() {
        let (dir, s) = store();
        let f = put(&s, dir.path(), b"hello world", "notes.txt").unwrap();
        assert!(valid_id(&f.id));
        assert_eq!(f.mime, "text/plain");
        let out = read(&s, dir.path(), &f).unwrap();
        assert_eq!(out["text"], json!("hello world"));
        assert_eq!(out["extractable"], json!(true));
    }

    #[test]
    fn put_is_idempotent_in_one_scope() {
        let (dir, s) = store();
        let a = put(&s, dir.path(), b"same", "a.txt").unwrap();
        let b = put(&s, dir.path(), b"same", "b.txt").unwrap();
        assert_eq!(a.id, b.id, "same bytes same scope dedupe to one row");
        let other = ChatFile::app_scope("inst-1", "conv-1").unwrap();
        let c = put_as(&s, dir.path(), &stage(dir.path(), b"same"), "a.txt", &other).unwrap();
        assert_ne!(a.id, c.id, "a different scope is a different row");
    }

    #[test]
    fn refuses_bad_magic_and_unknown_binary() {
        let (dir, s) = store();
        let refused =
            |bytes: &[u8], name: &str| put(&s, dir.path(), bytes, name).unwrap_err().to_string();
        // A .pdf whose bytes are not a PDF refuses: a name is not a kind.
        assert!(refused(b"hello", "fake.pdf").contains("sniff"));
        // A .png whose bytes are a ZIP magic refuses on the sniff.
        assert!(refused(b"PK\x03\x04rest", "a.png").contains("sniff"));
        // SVG bytes mislabelled as text refuse on the sniff.
        assert!(refused(b"<svg></svg>", "x.txt").contains("sniff"));
        // Unsigned binary bytes have no attachable kind.
        assert!(refused(b"MZ", "a.exe").contains("not an attachable kind"));
        // With no extension prefilter the bytes decide: plain UTF-8 is
        // ordinary text even under an unrecognised name.
        assert_eq!(
            put(&s, dir.path(), b"plain notes", "NOTES").unwrap().mime,
            "text/plain"
        );
    }

    /// A minimal, well-formed one-page PDF whose only text is `text`. Built
    /// here rather than embedded as a fixture, so the test exercises the real
    /// converter against a real document instead of asserting that a blob
    /// happens to pass.
    fn minimal_pdf(text: &str) -> Vec<u8> {
        let content = format!("BT /F1 24 Tf 20 100 Td ({text}) Tj ET\n");
        let objects = [
            "<</Type/Catalog/Pages 2 0 R>>".to_string(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>".to_string(),
            format!("<</Length {}>>\nstream\n{content}endstream", content.len()),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// The PDF path end to end: a real document uploads, and reading it back
    /// returns the text the converter recovered — not a metadata-only row.
    #[test]
    fn a_real_pdf_uploads_and_reads_back_its_text() {
        let (dir, s) = store();
        let file = put(&s, dir.path(), &minimal_pdf("Invoice 42"), "invoice.pdf").unwrap();
        assert_eq!(file.mime, "application/pdf");
        let out = read(&s, dir.path(), &file).unwrap();
        assert_eq!(
            out["text"].as_str().unwrap_or_default().trim(),
            "Invoice 42"
        );
        assert_eq!(out["extractable"], json!(true));
    }

    /// A host without the converter says so and names the remedy; it must not
    /// read as a fault in the user's file.
    #[test]
    fn a_missing_pdftotext_is_reported_as_missing_not_as_a_bad_pdf() {
        let err = pdf_attachment_text(
            "cadence-no-such-pdftotext",
            "invoice.pdf",
            &minimal_pdf("Invoice 42"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("`cadence-no-such-pdftotext` is not installed"),
            "{err}"
        );
        assert!(err.contains("poppler-utils"), "{err}");
        assert!(!err.contains("could not be read"), "{err}");
        assert!(!err.contains("corrupt"), "{err}");
    }

    /// A PDF whose bytes are a PDF but which no converter can read is refused
    /// at upload, so it never becomes an attachment the assistant cannot read.
    #[test]
    fn pdf_is_read_only_when_its_text_extracts() {
        let (dir, s) = store();
        let err = put(
            &s,
            dir.path(),
            b"%PDF-1.4\nnot a real document",
            "broken.pdf",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("corrupt, encrypted"), "{err}");
    }

    /// The structured text kinds are retained as text and read back verbatim.
    #[test]
    fn structured_text_kinds_are_readable() {
        let (dir, s) = store();
        let cases: [(&str, &[u8], &str); 6] = [
            ("data.json", br#"{"total":42}"#, "application/json"),
            (
                "data.jsonl",
                b"{\"a\":1}\n{\"b\":2}\n",
                "application/x-ndjson",
            ),
            ("conf.yaml", b"total: 100\n", "application/yaml"),
            ("conf.yml", b"total: 100\n", "application/yaml"),
            ("rows.tsv", b"a\tb\n1\t2\n", "text/tab-separated-values"),
            ("feed.xml", b"<rss><item>x</item></rss>", "application/xml"),
        ];
        for (name, bytes, mime) in cases {
            let file = put(&s, dir.path(), bytes, name).unwrap();
            assert_eq!(file.mime, mime, "{name}");
            let out = read(&s, dir.path(), &file).unwrap();
            assert_eq!(out["text"], json!(String::from_utf8_lossy(bytes)), "{name}");
        }
    }

    #[test]
    fn refuses_oversize_and_empty() {
        let (dir, s) = store();
        let big = vec![b'x'; (CHAT_FILE_MAX_BYTES + 1) as usize];
        let err = put(&s, dir.path(), &big, "big.txt")
            .unwrap_err()
            .to_string();
        assert!(err.contains("cap"), "{err}");
        let err = put(&s, dir.path(), b"", "e.txt").unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn refuses_tmp_outside_staging() {
        let (dir, s) = store();
        let elsewhere = dir.path().join("elsewhere.txt");
        std::fs::write(&elsewhere, b"hi").unwrap();
        let err = put_as(&s, dir.path(), &elsewhere, "e.txt", CHAT_FILE_SCOPE_HOME)
            .unwrap_err()
            .to_string();
        assert!(err.contains("directly under"), "{err}");
    }

    #[test]
    fn altered_blob_refuses_and_missing_blob_does_not_fall_back() {
        let (dir, s) = store();
        let f = put(&s, dir.path(), b"data", "d.txt").unwrap();
        let blob = workspace(dir.path())
            .join(".cadence")
            .join(CHAT_FILES_DIR)
            .join(&f.sha256);
        // Corrupt the blob after landing.
        std::fs::write(&blob, b"tampered").unwrap();
        let err = read(&s, dir.path(), &f).unwrap_err().to_string();
        assert!(err.contains("digest"), "{err}");
        // A state-dir copy is never consulted: removing the workspace blob
        // refuses even with the right bytes sitting in `<state>/chat-files`.
        std::fs::remove_file(&blob).unwrap();
        let legacy = dir.path().join(CHAT_FILES_DIR);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join(&f.sha256), b"data").unwrap();
        let err = read(&s, dir.path(), &f).unwrap_err().to_string();
        assert!(err.contains("missing"), "{err}");
    }

    #[test]
    fn csv_and_markdown_are_text_kinds() {
        let (dir, s) = store();
        let csv = put(&s, dir.path(), b"a,b\n1,2", "ROWS.CSV").unwrap();
        assert_eq!(csv.mime, "text/csv");
        let md = put(&s, dir.path(), b"# hi", "n.md").unwrap();
        assert_eq!(md.mime, "text/markdown");
        let out = read(&s, dir.path(), &csv).unwrap();
        assert_eq!(out["text"], json!("a,b\n1,2"));
    }

    /// The Home operator read goes straight from the digest check to
    /// `read_projection`, never through the readiness helper. The kind proof
    /// therefore has to live in the projection too, or a row whose label and
    /// bytes disagree would be served as text on that one path.
    #[test]
    fn a_row_whose_label_disagrees_with_its_bytes_is_refused_on_read() {
        let bytes = b"%PDF-1.4\n".to_vec();
        let file = ChatFile {
            id: format!("chf-{}", "a".repeat(32)),
            sha256: sha256_hex(&bytes),
            size: bytes.len() as u64,
            mime: "application/json".to_string(),
            name: "payload.json".to_string(),
            scope: CHAT_FILE_SCOPE_HOME.to_string(),
            context_id: String::new(),
            uploader: format!("acu-{}", "b".repeat(32)),
            user_id: None,
            thread_id: None,
            location_version: None,
            created: 0.0,
        };
        let err = file.read_projection(bytes).unwrap_err().to_string();
        assert!(err.contains("do not match its declared kind"), "{err}");
    }

    #[test]
    fn names_are_sanitized() {
        let (dir, s) = store();
        let f = put(&s, dir.path(), b"x", "../evil/\u{1}\u{2}name.txt").unwrap();
        assert_eq!(f.name, "name.txt");
    }

    /// A multibyte character straddling byte 8192 is valid UTF-8, not
    /// binary: the head sniff trims to the boundary. Genuinely invalid
    /// bytes in the same place still refuse.
    #[test]
    fn multibyte_char_across_sniff_window_uploads_invalid_bytes_refuse() {
        let (dir, s) = store();
        let mut ok = vec![b'a'; 8191];
        ok.extend_from_slice("中文\n".as_bytes());
        let f = put(&s, dir.path(), &ok, "names.csv").unwrap();
        let out = read(&s, dir.path(), &f).unwrap();
        assert_eq!(out["text"].as_str().unwrap().len(), ok.len());
        let mut bad = vec![b'a'; 8191];
        bad.extend_from_slice(&[0xff, b'\n']);
        let err = put(&s, dir.path(), &bad, "bad.csv")
            .unwrap_err()
            .to_string();
        assert!(err.contains("sniff"), "{err}");
    }

    /// The workspace is the tracker's git tree: after an upload the
    /// custody paths are git-ignored, so no tracker commit or `git add -A`
    /// can pick up customer bytes.
    #[test]
    fn custody_is_git_ignored_in_the_workspace_repo() {
        let (dir, s) = store();
        let ws = workspace(dir.path());
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&ws)
                .args(args)
                .output()
                .unwrap()
        };
        assert!(git(&["init", "-q"]).status.success());
        let f = put(&s, dir.path(), b"a,b\n1,2", "rows.csv").unwrap();
        let status = git(&["status", "--porcelain", "--untracked-files=all"]);
        assert!(
            status.stdout.is_empty(),
            "{:?}",
            String::from_utf8_lossy(&status.stdout)
        );
        let blob = format!(".cadence/{CHAT_FILES_DIR}/{}", f.sha256);
        assert!(git(&["check-ignore", "-q", &blob]).status.success());
        // A pre-existing rule that is not exactly `*` is rewritten, and a
        // symlinked `.gitignore` is replaced, never followed.
        let rule = ws.join(".cadence").join(CHAT_FILES_DIR).join(".gitignore");
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, b"keep").unwrap();
        std::fs::remove_file(&rule).unwrap();
        std::os::unix::fs::symlink(&outside, &rule).unwrap();
        put(&s, dir.path(), b"c,d\n3,4", "more.csv").unwrap();
        assert_eq!(std::fs::read(&rule).unwrap(), b"*\n");
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        std::fs::write(&rule, b"# nothing\n").unwrap();
        put(&s, dir.path(), b"e,f\n5,6", "again.csv").unwrap();
        assert_eq!(std::fs::read(&rule).unwrap(), b"*\n");
    }
}
