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

/// Custody leaf under `<workspace>/.cadence/`: `chat-files/`.
pub const CHAT_FILES_DIR: &str = "chat-files";

/// One file is at most 10 MiB (the mock's ceiling; the multipart
/// envelope rides above it at the board).
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
            "created": self.created,
        })
    }

    /// The read projection of one row from the exact bytes whose size
    /// and digest were verified against it: bounded secret-scanned UTF-8
    /// text (only text kinds are ever retained). The caller passes the
    /// checked bytes; this helper never
    /// opens a path, so a completed scope or capability proof is not
    /// undone by a second read.
    pub fn read_projection(&self, bytes: Vec<u8>) -> Result<Value> {
        if !text_kind(&self.mime) {
            return Err(Error::rejected(format!(
                "attachment '{}' is not a text attachment",
                self.id
            )));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::rejected(format!("attachment '{}' is not UTF-8 text", self.id)))?;
        // The read re-runs the write-time gate: a blob that would not
        // pass `secret::guard` today is withheld, never served.
        crate::secret::guard(&format!("chat-file/{}", self.name), &text)?;
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
    pub workspace_dir: &'a Path,
}

/// The daemon-minted id grammar — `chf-` + 32 lowercase hex. Checked at
/// every caller-supplied boundary so a forged id never reaches a path.
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

/// What the real bytes sniff as, restricted to the v1 allowlist plus
/// the honest refusal classes (executables, archives, svg/html, zip).
/// `sniff_mime` is the wiki's magic-byte table.
fn sniffed_kind(bytes: &[u8]) -> &'static str {
    match crate::wiki::sniff_mime(bytes) {
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" => "jpeg",
        "image/webp" => "webp",
        // `sniff_mime` folds every UTF-8 file to text/plain except the
        // html/svg shapes it names — those are refused below.
        "text/plain" => "text",
        other => other,
    }
}

/// The claimed extension must match the real bytes; `mime` is the
/// stored content type. Refuses anything off the allowlist
/// (executables, html/svg, archives, corrupt/empty magic). PDF and
/// image kinds — by extension or by sniffed bytes — are refused with a
/// specific reason: their processing is a separate ticket (CAD-1233).
fn check_kind(name: &str, bytes: &[u8]) -> Result<&'static str> {
    let ext = claimed_ext(name);
    let ext = ext.as_str();
    let sniffed = sniffed_kind(bytes);
    if matches!(ext, "pdf" | "png" | "jpg" | "jpeg" | "webp")
        || matches!(sniffed, "pdf" | "png" | "jpeg" | "webp")
    {
        return Err(Error::rejected(format!(
            "chat_file_upload refused '{name}': PDF and image processing is not \
             available yet — the interim allowlist is txt, md and csv",
        )));
    }
    let allowed = matches!(ext, "txt" | "md" | "csv");
    if !allowed {
        return Err(Error::rejected(format!(
            "chat_file_upload: '.{ext}' is not an attachable type — the interim \
             allowlist is txt, md and csv",
        )));
    }
    let mime = match ext {
        "txt" | "md" | "csv" if sniffed == "text" => match ext {
            "csv" => "text/csv",
            "md" => "text/markdown",
            _ => "text/plain",
        },
        _ => {
            return Err(Error::rejected(format!(
                "chat_file_upload refused '{name}': a '.{ext}' file must sniff as \
                 its declared kind — the bytes did not (extension and client MIME \
                 are never the enforcement boundary)"
            )));
        }
    };
    Ok(mime)
}

/// `true` for the kinds `chat_file_read` returns text for; everything
/// else is metadata-only in v1 (no PDF/OCR/image extraction).
pub fn text_kind(mime: &str) -> bool {
    matches!(mime, "text/plain" | "text/markdown" | "text/csv")
}

impl Store {
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
        let head = &bytes[..bytes.len().min(8192)];
        let mime = check_kind(&name, head)?.to_string();
        // Text kinds carry the credential scan before retention — the
        // same gate as a wiki text write.
        if text_kind(&mime) {
            let text = std::str::from_utf8(&bytes).map_err(|e| {
                Error::rejected(format!("chat_file_upload '{name}': not UTF-8 text: {e}"))
            })?;
            crate::secret::guard(&format!("chat-file/{name}"), text)?;
        }

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
        enforce_workspace_quota(self, &root, scope, bytes.len() as u64)?;

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

    fn chat_file_by_sha_in(
        tx: &impl super::StoreConn,
        sha256: &str,
        scope: &str,
        context_id: &str,
    ) -> Result<Option<ChatFile>> {
        Ok(tx
            .query_row(
                "SELECT id,sha256,size,mime,name,scope,context_id,uploader,created
                 FROM chat_files WHERE sha256=? AND scope=? AND context_id=?",
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
                "SELECT id,sha256,size,mime,name,scope,context_id,uploader,created
                 FROM chat_files WHERE id=?",
                [id],
                row_chat_file,
            )
            .optional()?)
    }

    /// The entries `thread_send` names in `attachments`, resolved to
    /// metadata rows — every id must exist (the daemon checked the
    /// grammar); an unknown id refuses the send. Order preserved.
    /// Existence is NOT readiness: `thread_send` resolves through
    /// [`Store::chat_file_ready`], which checks the actual bytes.
    pub fn chat_files_for(&self, ids: &[String]) -> Result<Vec<ChatFile>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let file = self
                .chat_file(id)?
                .ok_or_else(|| Error::rejected(format!("unknown attachment '{id}'")))?;
            out.push(file);
        }
        Ok(out)
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

    /// Checked readiness for one retained text row against the bytes on
    /// disk: the row must be a text kind within `cap` (clamped to
    /// [`CHAT_FILE_MAX_BYTES`]; a larger row refuses, never truncates),
    /// the workspace blob must exist as a regular file and its bounded
    /// content must still match the declared size and digest, sniff as an
    /// allowed text kind, decode as UTF-8 and pass the secret gate.
    /// Returns the exact checked bytes. This proves bytes only — scope,
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
        if !text_kind(&file.mime) {
            return Err(Error::rejected(format!(
                "attachment '{}' is not a ready text source — only txt, md and csv \
                 rows can be referenced",
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

    fn chat_file_ready_checked_bytes(&self, file: &ChatFile, bytes: Vec<u8>) -> Result<Vec<u8>> {
        if bytes.is_empty() {
            return Err(Error::rejected(format!(
                "attachment '{}' is not ready — the retained file is empty",
                file.id
            )));
        }
        // The same first-block sniff the upload ran: a row whose bytes
        // now shape as html/svg is not an allowed text source.
        let head = &bytes[..bytes.len().min(8192)];
        if sniffed_kind(head) != "text" {
            return Err(Error::rejected(format!(
                "attachment '{}' is not ready — the retained bytes do not sniff as \
                 the allowed text kinds",
                file.id
            )));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            Error::rejected(format!(
                "attachment '{}' is not ready — the retained bytes are not UTF-8 text",
                file.id
            ))
        })?;
        crate::secret::guard(&format!("chat-file/{}", file.name), text).map_err(|e| {
            Error::rejected(format!(
                "attachment '{}' is not ready — the retained text failed the secret \
                 scan: {e}",
                file.id
            ))
        })?;
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

/// The workspace is the tracker's git working tree: keep custody bytes out
/// of it with a `*` ignore rule inside the custody directory, created
/// exclusively through the pinned descriptor (never following a symlink).
/// An existing entry is left as is.
#[cfg(unix)]
fn ensure_custody_gitignore(dir: std::os::fd::RawFd) -> Result<()> {
    let fd = unsafe {
        libc::openat(
            dir,
            c".gitignore".as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EEXIST) {
            return Ok(());
        }
        return Err(Error::rejected(format!(
            "cannot create the custody ignore rule: {e}"
        )));
    }
    use std::io::Write;
    use std::os::fd::FromRawFd;
    let mut file = std::fs::File::from(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
    file.write_all(b"*\n")?;
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
    name: Vec<u8>,
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
            name: raw.to_bytes().to_vec(),
            device: stat.st_dev,
            inode: stat.st_ino,
            size: stat.st_size as u64,
        });
    }
    Ok(files)
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
        "SELECT id,sha256,size,mime,name,scope,context_id,uploader,created FROM chat_files",
    )?;
    let mut rows = stmt.query([])?;
    let mut ledger = QuotaMetadataLedger {
        known_blobs: std::collections::HashSet::new(),
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
        ledger
            .known_blobs
            .insert((file.sha256.as_bytes().to_vec(), file.size));
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
    scope: &str,
    new_size: u64,
) -> Result<()> {
    const INSTANCE_BYTES: u64 = 1024 * 1024 * 1024;
    const INSTANCE_OBJECTS: u64 = 1000;
    const BUCKET_BYTES: u64 = 256 * 1024 * 1024;
    const BUCKET_OBJECTS: u64 = 250;
    #[cfg(unix)]
    {
        let observed = quota_scan_dir(root.fd.as_raw_fd())?;
        let ledger = quota_metadata_ledger(store, scope)?;
        let known_blobs = ledger.known_blobs;
        let bucket_bytes = ledger.bucket_bytes;
        let bucket_metadata = ledger.bucket_objects;
        let metadata_objects = ledger.metadata_objects;
        let mut physical =
            std::collections::HashMap::<(libc::dev_t, libc::ino_t), (u64, bool)>::new();
        for file in observed {
            let known = known_blobs.contains(&(file.name, file.size));
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
    use crate::store::NewAgent;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
        (dir, s)
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
    fn refuses_bad_magic_and_offlist() {
        let (dir, s) = store();
        let refused =
            |bytes: &[u8], name: &str| put(&s, dir.path(), bytes, name).unwrap_err().to_string();
        // PDF/image claims are refused while no processing contract
        // exists — a magic prefix is not processing.
        assert!(refused(b"hello", "fake.pdf").contains("not available yet"));
        // a zip named .png is refused the same way (no image processing)
        assert!(refused(b"PK\x03\x04rest", "a.png").contains("not available yet"));
        // svg is text-ish by shape but off the allowlist
        assert!(refused(b"<svg></svg>", "x.txt").contains("sniff"));
        // an unsupported extension refuses before sniffing
        assert!(refused(b"MZ", "a.exe").contains("not an attachable type"));
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

    #[test]
    fn names_are_sanitized() {
        let (dir, s) = store();
        let f = put(&s, dir.path(), b"x", "../evil/\u{1}\u{2}name.txt").unwrap();
        assert_eq!(f.name, "name.txt");
    }

    // The thread_send seam's round-trip lives in the store tests for
    // threads; registration required.
    #[test]
    fn chat_files_for_resolves_in_order() {
        let (dir, s) = store();
        s.register_agent(&NewAgent {
            alias: "master",
            provider: "fake",
            endpoint_kind: "managed",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        let a = put(&s, dir.path(), b"1", "a.txt").unwrap();
        let b = put(&s, dir.path(), b"2", "b.txt").unwrap();
        let got = s.chat_files_for(&[b.id.clone(), a.id.clone()]).unwrap();
        assert_eq!(got[0].id, b.id);
        assert_eq!(got[1].id, a.id);
        assert!(s.chat_files_for(&["chf-missing".into()]).is_err());
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
    }
}
