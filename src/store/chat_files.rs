//! CAD-1168 slice 2: host-custodied chat attachments.
//!
//! A retained file the operator attaches to a `thread_send` message.
//! Bytes live content-addressed under `<workspace>/.cadence/chat-files/<sha256>`
//! for daemon writes; the legacy Store entry points retain their historical
//! `<state>/chat-files/` behavior for existing callers and fixtures.
//! this table is the metadata index — the daemon-minted `id` is the
//! only handle any caller ever sees, so no path, name or hash is a
//! reference the client supplies at read or send time.
//!
//! The caller's staging path is only a name under the server-owned
//! `<state>/wiki-uploads/` directory: the source is read through that
//! directory's own descriptor (never through the caller's alias), and
//! the private custody stage is owned only once its exclusive create
//! succeeded.
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
//!   Legacy `home` rows are never relabeled as app rows, and a stored
//!   label that is neither `home` nor a well-formed app scope refuses.

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

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

/// Blob dir under the state dir: `<state>/chat-files/`.
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
    /// and digest were verified against it: a non-text kind stays
    /// metadata-only (the bytes are still verified, never fabricated
    /// into text), a text kind returns bounded secret-scanned UTF-8
    /// text. The caller passes the checked bytes; this helper never
    /// opens a path, so a completed scope or capability proof is not
    /// undone by a second read.
    pub fn read_projection(&self, bytes: Vec<u8>) -> Result<Value> {
        if !text_kind(&self.mime) {
            return Ok(json!({
                "id": self.id,
                "name": self.name,
                "size": self.size,
                "mime": self.mime,
                "sha256": self.sha256,
                "extractable": false,
                "text": Value::Null,
                "note": "this kind is metadata-only in v1 — no PDF, image or \
                         OCR extraction runs on attachments",
            }));
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

/// A path this request itself created under its own custody staging,
/// removed on every return path — success, refusal or unwind. The
/// guard is constructed only after `create_new` succeeded, so it is
/// proof this call owns the path: a pre-existing or colliding stage is
/// never deleted or truncated. A caller-supplied source `tmp` is never
/// wrapped — the store did not create it.
struct StagedFile(PathBuf);

/// Explicit storage authority passed by native callers. Grouping the two
/// roots keeps daemon adapter signatures clear without hiding authority in a flag.
pub struct ChatFileStorageRoots<'a> {
    pub state_dir: &'a Path,
    pub workspace_dir: &'a Path,
}

struct ChatFileUpload<'a> {
    tmp: &'a Path,
    name: &'a str,
    scope: &'a str,
    context_id: &'a str,
    uploader: &'a str,
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
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
fn claimed_ext(name: &str) -> &str {
    name.rsplit_once('.').map(|(_, e)| e).unwrap_or("")
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
    /// route, or a direct RPC caller) owns that cleanup. The store
    /// owns only its private custody stage, and only after the
    /// exclusive create that minted it.
    ///
    /// The source is read with a bounded no-follow/nonblocking open
    /// (at most [`CHAT_FILE_MAX_BYTES`] + 1 bytes), the exact bytes
    /// read are written to private staging, and only those bytes are
    /// published — the checked bytes are the published bytes. The
    /// digest, kind, UTF-8 and secret checks run before publication.
    ///
    /// `scope` is the stored provenance label the daemon derived, never
    /// a request field: `home` for the operator's own chat, or
    /// [`ChatFile::app_scope`]'s encoding of the verified installation
    /// and conversation with the proven context in `context_id`.
    /// Idempotent by (sha256, scope, context_id): same bytes in the same
    /// scope return the same row; different scope is a different row.
    pub fn chat_file_put(
        &self,
        state_dir: &Path,
        tmp: &Path,
        name: &str,
        scope: &str,
        context_id: &str,
        uploader: &str,
    ) -> Result<ChatFile> {
        self.chat_file_put_at(
            state_dir,
            &state_dir.join(CHAT_FILES_DIR),
            ChatFileUpload {
                tmp,
                name,
                scope,
                context_id,
                uploader,
            },
            None,
        )
    }

    /// Workspace-authoritative variant used by daemon RPCs. The upload
    /// source remains transient state-dir staging; retained bytes never
    /// fall back to state custody when workspace custody is unavailable.
    pub fn chat_file_put_in_workspace(
        &self,
        roots: ChatFileStorageRoots<'_>,
        tmp: &Path,
        name: &str,
        scope: &str,
        context_id: &str,
        uploader: &str,
    ) -> Result<ChatFile> {
        let root = workspace_blob_dir(roots.workspace_dir, true)?
            .ok_or_else(|| Error::rejected("workspace custody directory is unavailable"))?;
        self.chat_file_put_at(
            roots.state_dir,
            &root.path,
            ChatFileUpload {
                tmp,
                name,
                scope,
                context_id,
                uploader,
            },
            Some(&root),
        )
    }

    fn chat_file_put_at(
        &self,
        state_dir: &Path,
        dir: &Path,
        upload: ChatFileUpload<'_>,
        pinned: Option<&PinnedBlobDir>,
    ) -> Result<ChatFile> {
        let ChatFileUpload {
            tmp,
            name,
            scope,
            context_id,
            uploader,
        } = upload;
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
        let _quota_lock = pinned.map(acquire_workspace_quota_lock).transpose()?;

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

        if let Some(root) = pinned {
            let existing = {
                let conn = self.conn();
                Self::chat_file_by_sha_in(&*conn, &sha256, scope, context_id)?
            };
            if let Some(existing) = existing {
                if let Some(stored) = open_blob_at(root, &existing.sha256)? {
                    let actual = read_bounded_file(&stored, CHAT_FILE_MAX_BYTES)?;
                    if actual.len() as u64 != existing.size
                        || sha256_hex(&actual) != existing.sha256
                    {
                        return Err(Error::rejected(
                            "existing workspace attachment bytes failed digest verification",
                        ));
                    }
                    return Ok(existing);
                }
            }
            enforce_workspace_quota(self, state_dir, root, scope, bytes.len() as u64)?;
        }

        // Private custody staging: write exactly the checked bytes and
        // re-read them bounded, so what is published is provably what
        // was checked. The stage is this call's own uuid-named file and
        // is removed on every return path.
        create_blob_dir(dir)?;
        let stage_at = pinned
            .map(|root| write_stage_at(root, &bytes))
            .transpose()?;
        let stage = if stage_at.is_none() {
            Some(dir.join(format!(".tmp-{}", uuid::Uuid::new_v4().simple())))
        } else {
            None
        };
        let _stage_guard = stage
            .as_ref()
            .map(|path| write_stage(path, &bytes))
            .transpose()?;
        let staged = if let Some(stage) = stage_at.as_ref() {
            stage.read_back(CHAT_FILE_MAX_BYTES)?
        } else {
            read_bounded(
                stage.as_deref().expect("legacy stage exists"),
                CHAT_FILE_MAX_BYTES,
            )?
            .ok_or_else(|| {
                Error::rejected("chat_file_upload refused: the custody stage vanished")
            })?
        };
        if staged != bytes {
            return Err(Error::rejected(
                "chat_file_upload refused: the custody stage did not match the checked bytes",
            ));
        }

        // Publish content-addressed, never overwriting and never
        // directly at the final name: a hard link from the private
        // stage is atomic, and a racing writer that landed the same
        // digest first wins — the existing blob is then verified and a
        // mismatch refuses the upload rather than replacing or
        // deleting it. A filesystem without hard links refuses rather
        // than exposing a partial blob under the final digest name.
        if let Some(stage) = stage_at.as_ref() {
            publish_blob_at(stage, &sha256, bytes.len() as u64)?;
        } else {
            let stage = stage.as_deref().expect("legacy stage exists");
            let dest = dir.join(&sha256);
            publish_blob(stage, &dest, &sha256, bytes.len() as u64)?;
        }

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

    /// `chat_file_read` (the daemon checked the caller): text kinds
    /// return bounded UTF-8 text with the digest re-verified and the
    /// secret scan re-run; pdf/images return metadata with
    /// `extractable:false` — an honest refusal to fabricate rather than
    /// a mock extraction. Raw binary is never returned.
    pub fn chat_file_read(&self, state_dir: &Path, id: &str) -> Result<Value> {
        let file = self
            .chat_file(id)?
            .ok_or_else(|| Error::rejected(format!("unknown attachment '{id}'")))?;
        let path = blob_path_for(state_dir, &file.sha256)?;
        let bytes = read_verified_blob(&path, &file.sha256, file.size)
            .map_err(|e| {
                Error::rejected(format!("attachment '{id}' is unreadable or altered: {e}"))
            })?
            .ok_or_else(|| {
                Error::rejected(format!(
                    "attachment '{id}' is unreadable or altered: the retained blob is missing"
                ))
            })?;
        file.read_projection(bytes)
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

    /// [`Store::chat_file_read`] for a row the caller already resolved
    /// and proved: the same digest-verified read and projection, without
    /// looking the row up again. The caller's proof is not undone by a
    /// second lookup, and the bytes served are the bytes verified here.
    pub fn chat_file_read_row(&self, state_dir: &Path, file: &ChatFile) -> Result<Value> {
        let path = blob_path_for(state_dir, &file.sha256)?;
        self.chat_file_read_row_path(file, &path)
    }

    /// Workspace-rooted read with non-destructive legacy fallback. Only
    /// a genuinely absent primary entry permits reading state custody;
    /// an existing symlink, FIFO, directory, or corrupt blob refuses.
    pub fn chat_file_read_row_in_workspace(
        &self,
        state_dir: &Path,
        workspace_dir: &Path,
        file: &ChatFile,
    ) -> Result<Value> {
        let digest = validated_digest(&file.sha256)?;
        let bytes = match workspace_blob_dir(workspace_dir, false)? {
            Some(root) => read_verified_at(&root, digest, file.size)?,
            None => None,
        };
        let bytes = match bytes {
            Some(bytes) => bytes,
            None => read_from_legacy_root(state_dir, digest, file.size)?
                .ok_or_else(|| Error::rejected("legacy retained blob is missing"))?,
        };
        file.read_projection(bytes)
    }

    fn chat_file_read_row_path(&self, file: &ChatFile, path: &Path) -> Result<Value> {
        let bytes = read_verified_blob(path, &file.sha256, file.size)
            .map_err(|e| {
                Error::rejected(format!(
                    "attachment '{}' is unreadable or altered: {e}",
                    file.id
                ))
            })?
            .ok_or_else(|| {
                Error::rejected(format!(
                    "attachment '{}' is unreadable or altered: the retained blob is missing",
                    file.id
                ))
            })?;
        file.read_projection(bytes)
    }

    /// Checked readiness for one retained text source, against the bytes
    /// on disk: the row must be a text kind, the blob must exist as a
    /// regular file, and its bounded content must still match the
    /// declared size and digest, sniff as an allowed text kind, decode as
    /// UTF-8 and pass the secret gate. Returns the row only when every
    /// check holds — existence-only [`Store::chat_files_for`] is not
    /// readiness, and a metadata-only PDF/image row is never usable as
    /// ready text. The caller may then reference the row.
    pub fn chat_file_ready(&self, state_dir: &Path, id: &str) -> Result<ChatFile> {
        self.chat_file_ready_bytes(state_dir, id, CHAT_FILE_MAX_BYTES)
            .map(|(file, _)| file)
    }

    /// [`Store::chat_file_ready`] with the exact bytes that passed every
    /// check, bounded by `cap` (itself clamped to
    /// [`CHAT_FILE_MAX_BYTES`]). A caller that needs a smaller window
    /// (the CRM CSV import's 256 KiB) passes it and gets a refusal when
    /// the retained row is larger — never a silently truncated read. The
    /// bytes returned are the bytes whose size and digest were verified,
    /// and the caller still holds no path: the blob is derived from the
    /// validated stored digest inside this store.
    pub fn chat_file_ready_bytes(
        &self,
        state_dir: &Path,
        id: &str,
        cap: u64,
    ) -> Result<(ChatFile, Vec<u8>)> {
        let file = self
            .chat_file(id)?
            .ok_or_else(|| Error::rejected(format!("unknown attachment '{id}'")))?;
        let bytes = self.chat_file_ready_checked(state_dir, &file, cap)?;
        Ok((file, bytes))
    }

    /// [`Store::chat_file_ready_bytes`] for a row the caller already
    /// resolved: the same byte-level readiness checks (text kind, cap,
    /// digest/size, sniff, UTF-8, secret policy) against the exact
    /// bytes whose size and digest the row declares. This proves bytes
    /// only — scope, provenance and capability stay the caller's proof,
    /// and a caller holding an authorized operation must call this
    /// while that operation is still open.
    pub fn chat_file_ready_checked(
        &self,
        state_dir: &Path,
        file: &ChatFile,
        cap: u64,
    ) -> Result<Vec<u8>> {
        let path = blob_path_for(state_dir, &file.sha256)?;
        self.chat_file_ready_checked_path(file, cap, &path)
    }

    /// Workspace-rooted readiness adapter used by native scoped reads.
    /// A present but invalid workspace entry is an error and is never
    /// hidden by a same-digest legacy state blob.
    pub fn chat_file_ready_checked_in_workspace(
        &self,
        state_dir: &Path,
        workspace_dir: &Path,
        file: &ChatFile,
        cap: u64,
    ) -> Result<Vec<u8>> {
        let digest = validated_digest(&file.sha256)?;
        let bytes = match workspace_blob_dir(workspace_dir, false)? {
            Some(root) => read_verified_at(&root, digest, file.size)?,
            None => None,
        };
        let bytes = match bytes {
            Some(bytes) => bytes,
            None => read_from_legacy_root(state_dir, digest, file.size)?
                .ok_or_else(|| Error::rejected("legacy retained blob is missing"))?,
        };
        self.chat_file_ready_checked_bytes(file, cap, bytes)
    }

    fn chat_file_ready_checked_path(
        &self,
        file: &ChatFile,
        cap: u64,
        path: &Path,
    ) -> Result<Vec<u8>> {
        let cap = cap.min(CHAT_FILE_MAX_BYTES);
        if !text_kind(&file.mime) {
            return Err(Error::rejected(format!(
                "attachment '{}' is not a ready text source — only txt, md and csv \
                 rows can be referenced; PDF/image rows stay metadata-only",
                file.id
            )));
        }
        if file.size > cap {
            return Err(Error::rejected(format!(
                "attachment '{}' is {} bytes — over the {cap}-byte read cap",
                file.id, file.size
            )));
        }
        let bytes = read_verified_blob(path, &file.sha256, file.size)
            .map_err(|e| {
                Error::rejected(format!(
                    "attachment '{}' is not ready — the retained bytes failed their \
                     checks: {e}",
                    file.id
                ))
            })?
            .ok_or_else(|| {
                Error::rejected(format!(
                    "attachment '{}' is not ready — the retained bytes are missing",
                    file.id
                ))
            })?;
        self.chat_file_ready_checked_bytes(file, cap, bytes)
    }

    fn chat_file_ready_checked_bytes(
        &self,
        file: &ChatFile,
        cap: u64,
        bytes: Vec<u8>,
    ) -> Result<Vec<u8>> {
        let cap = cap.min(CHAT_FILE_MAX_BYTES);
        if !text_kind(&file.mime) {
            return Err(Error::rejected(format!(
                "attachment '{}' is not a ready text source — only txt, md and csv \
                 rows can be referenced; PDF/image rows stay metadata-only",
                file.id
            )));
        }
        if file.size > cap {
            return Err(Error::rejected(format!(
                "attachment '{}' is {} bytes — over the {cap}-byte read cap",
                file.id, file.size
            )));
        }
        if bytes.is_empty() {
            return Err(Error::rejected(format!(
                "attachment '{}' is not ready — the retained file is empty",
                file.id
            )));
        }
        // The same first-block sniff the upload ran: a legacy row whose
        // bytes now shape as html/svg is not an allowed text source.
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

/// The blob path a row's digest names — derived only from a validated
/// stored digest, never from request data. A malformed digest refuses
/// before any path is built.
fn validated_digest(sha256: &str) -> Result<&str> {
    if !valid_digest(sha256) {
        return Err(Error::rejected(
            "the retained row carries a malformed digest — refusing to derive a path from it",
        ));
    }
    Ok(sha256)
}

fn blob_path_for(state_dir: &Path, sha256: &str) -> Result<PathBuf> {
    Ok(state_dir
        .join(CHAT_FILES_DIR)
        .join(validated_digest(sha256)?))
}

/// A directory descriptor pinned after a no-follow component walk. Every
/// workspace blob operation is relative to this handle, never its pathname.
struct PinnedBlobDir {
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
    path: PathBuf,
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
        Ok(Some(PinnedBlobDir {
            fd: current,
            path: workspace.join(".cadence").join(CHAT_FILES_DIR),
        }))
    }
    #[cfg(not(unix))]
    {
        let path = workspace.join(".cadence").join(CHAT_FILES_DIR);
        if create {
            std::fs::create_dir_all(&path)?;
        }
        if !path.is_dir() {
            return Ok(None);
        }
        Ok(Some(PinnedBlobDir { path }))
    }
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
fn read_from_legacy_root(state_dir: &Path, digest: &str, size: u64) -> Result<Option<Vec<u8>>> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let state = open_absolute_directory(state_dir)?;
    let leaf = std::ffi::CString::new(CHAT_FILES_DIR).unwrap();
    let fd = unsafe {
        libc::openat(
            state.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(Error::rejected(format!(
            "legacy custody directory is invalid: {error}"
        )));
    }
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 || stat.st_uid != unsafe { libc::geteuid() } {
        unsafe {
            libc::close(fd);
        }
        return Err(Error::rejected(
            "legacy custody directory is not owned by the daemon uid",
        ));
    }
    let root = PinnedBlobDir {
        fd: unsafe { OwnedFd::from_raw_fd(fd) },
        path: state_dir.join(CHAT_FILES_DIR),
    };
    read_verified_at(&root, digest, size)
}

#[cfg(not(unix))]
fn read_from_legacy_root(_state_dir: &Path, _digest: &str, _size: u64) -> Result<Option<Vec<u8>>> {
    Err(Error::rejected(
        "descriptor-pinned legacy fallback is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn open_absolute_directory(path: &Path) -> Result<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    if !path.is_absolute() {
        return Err(Error::rejected("custody root must be absolute"));
    }
    let slash = std::ffi::CString::new("/").unwrap();
    let fd = unsafe {
        libc::open(
            slash.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(Error::rejected("cannot open filesystem root"));
    }
    let mut current = unsafe { OwnedFd::from_raw_fd(fd) };
    for component in path.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(part) => {
                let name = std::ffi::CString::new(part.as_bytes())
                    .map_err(|_| Error::rejected("custody path is invalid"))?;
                let child = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if child < 0 {
                    return Err(Error::rejected(
                        "custody root is missing, aliased, or unavailable",
                    ));
                }
                current = unsafe { OwnedFd::from_raw_fd(child) };
            }
            _ => return Err(Error::rejected("custody root is not normalized")),
        }
    }
    Ok(current)
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
    original_root: bool,
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
fn quota_scan_dir(
    fd: std::os::fd::RawFd,
    workspace_root: bool,
    original_root: bool,
) -> Result<Vec<QuotaFile>> {
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
        unsafe {
            *libc::__errno_location() = 0;
        }
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
        if workspace_root && raw.to_bytes() == b".quota.lock" {
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
            original_root,
        });
    }
    Ok(files)
}

#[cfg(unix)]
fn quota_open_state_dir(state_dir: &Path, leaf: &str) -> Result<Option<OwnedFd>> {
    let state = open_absolute_directory(state_dir)?;
    let name = std::ffi::CString::new(leaf).unwrap();
    let fd = unsafe {
        libc::openat(
            state.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(Error::rejected(format!(
            "quota accounting cannot open {leaf}: {error}"
        )));
    }
    Ok(Some(unsafe { OwnedFd::from_raw_fd(fd) }))
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
            return Err(Error::rejected("chat attachment instance quota reached"));
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
    state_dir: &Path,
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
        let workspace_files = quota_scan_dir(root.fd.as_raw_fd(), true, true)?;
        let mut observed = workspace_files;
        for (leaf, is_original_root) in [(CHAT_FILES_DIR, true), (crate::wiki::UPLOAD_DIR, false)] {
            if let Some(dir) = quota_open_state_dir(state_dir, leaf)? {
                observed.extend(quota_scan_dir(dir.as_raw_fd(), false, is_original_root)?);
            }
        }
        let ledger = quota_metadata_ledger(store, scope)?;
        let known_blobs = ledger.known_blobs;
        let bucket_bytes = ledger.bucket_bytes;
        let bucket_metadata = ledger.bucket_objects;
        let metadata_objects = ledger.metadata_objects;
        let mut physical =
            std::collections::HashMap::<(libc::dev_t, libc::ino_t), (u64, bool)>::new();
        for file in observed {
            let known = file.original_root && known_blobs.contains(&(file.name, file.size));
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
            return Err(Error::rejected("chat attachment instance quota reached"));
        }
        if projected_bucket_bytes > BUCKET_BYTES || projected_bucket_objects > BUCKET_OBJECTS {
            return Err(Error::rejected("chat attachment scope quota reached"));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (
            store,
            state_dir,
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

fn create_blob_dir(dir: &Path) -> Result<()> {
    if dir.file_name().and_then(|n| n.to_str()) == Some(CHAT_FILES_DIR)
        && dir
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            == Some(".cadence")
    {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    Ok(())
}

/// Read one regular file with a hard cap, opened once with no-follow
/// and non-blocking flags: a symlink leaf is refused rather than
/// followed, and a FIFO/device cannot block the open (no writer) or be
/// read as if it were custody. `Ok(None)` when the path does not exist.
fn read_bounded(path: &Path, cap: u64) -> Result<Option<Vec<u8>>> {
    let f = match open_nofollow(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::rejected(format!("unreadable: {e}"))),
    };
    Ok(Some(read_bounded_file(&f, cap)?))
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

/// Open a path without following a leaf symlink and without blocking on
/// a FIFO/device with no writer. The flags are applied on Unix (the
/// supported target); elsewhere a plain open is used and the
/// descriptor's `is_file` check still refuses non-regular files.
fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::File::open(path)
    }
}

/// Create and write private custody staging: `create_new` (never an
/// existing path) is the ownership proof, and the guard is returned
/// only after it succeeds — a collision refuses without deleting the
/// occupied path, while a partial write of the path this call created
/// is still removed by the guard. The caller holds the guard across
/// every later fallible step (write, sync, re-read, publication, DB)
/// and on success too.
fn write_stage(stage: &Path, bytes: &[u8]) -> Result<StagedFile> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage)?;
    let guard = StagedFile(stage.to_path_buf());
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(guard)
}

/// Publish checked bytes at their content address without ever
/// replacing or deleting what is already there, and without ever
/// exposing a partial file under the final digest name. A hard link
/// from the same-directory private stage is the atomic primitive
/// (atomic on one filesystem, fails with `AlreadyExists` when the name
/// is taken). A filesystem without hard links refuses rather than
/// writing directly to the final name. A concurrent writer racing the
/// same digest wins the name; this call then verifies the existing
/// blob and refuses when it does not match.
#[cfg(unix)]
struct PinnedStage {
    root_fd: std::os::fd::OwnedFd,
    file: std::fs::File,
}

#[cfg(unix)]
impl PinnedStage {
    fn read_back(&self, cap: u64) -> Result<Vec<u8>> {
        use std::io::Seek;
        let mut file = self.file.try_clone()?;
        file.seek(std::io::SeekFrom::Start(0))?;
        read_bounded_file(&file, cap)
    }
}

#[cfg(unix)]
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

#[cfg(not(unix))]
fn write_stage_at(_root: &PinnedBlobDir, _bytes: &[u8]) -> Result<PinnedStage> {
    Err(Error::rejected(
        "descriptor-pinned workspace custody is unsupported on this platform",
    ))
}

#[cfg(unix)]
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
                    path: PathBuf::new()
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

#[cfg(unix)]
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

#[cfg(not(unix))]
fn publish_blob_at(_stage: &PinnedStage, _digest: &str, _size: u64) -> Result<()> {
    Err(Error::rejected(
        "descriptor-pinned workspace custody is unsupported on this platform",
    ))
}

fn publish_blob(stage: &Path, dest: &Path, sha256: &str, size: u64) -> Result<()> {
    match std::fs::hard_link(stage, dest) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if matches!(read_verified_blob(dest, sha256, size), Ok(Some(_))) {
                Ok(())
            } else {
                Err(Error::rejected(
                    "chat_file_upload refused: the retained blob for this content \
                     failed its digest check — refusing to overwrite it",
                ))
            }
        }
        Err(e) => Err(Error::rejected(format!(
            "chat_file_upload refused: could not publish the retained blob atomically \
             (a hard link from its own staging is required): {e}"
        ))),
    }
}

/// One retained blob verified against the digest and size its row
/// declares. The bytes returned are the very bytes that were hashed, so
/// no caller verifies one read and then uses another.
fn read_verified_blob(path: &Path, sha256: &str, size: u64) -> Result<Option<Vec<u8>>> {
    let Some(bytes) = read_bounded(path, CHAT_FILE_MAX_BYTES)? else {
        return Ok(None);
    };
    if bytes.len() as u64 != size || sha256_hex(&bytes) != sha256 {
        return Err(Error::rejected(
            "the retained bytes failed their digest check — they no longer match the row",
        ));
    }
    Ok(Some(bytes))
}

/// The blob path for a row — derived only from the verified sha, never
/// from a caller-supplied name.
#[cfg(test)]
fn blob_path(state_dir: &Path, sha256: &str) -> PathBuf {
    state_dir.join(CHAT_FILES_DIR).join(sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewAgent;
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

    #[test]
    fn put_retains_text_and_read_returns_it() {
        let (dir, s) = store();
        let tmp = stage(dir.path(), b"hello world");
        let f = s
            .chat_file_put(dir.path(), &tmp, "notes.txt", "home", "", "operator")
            .unwrap();
        assert!(valid_id(&f.id));
        assert_eq!(f.mime, "text/plain");
        let out = s.chat_file_read(dir.path(), &f.id).unwrap();
        assert_eq!(out["text"], json!("hello world"));
        assert_eq!(out["extractable"], json!(true));
    }

    #[test]
    fn put_is_idempotent_in_one_scope() {
        let (dir, s) = store();
        let a = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"same"),
                "a.txt",
                "home",
                "",
                "operator",
            )
            .unwrap();
        let b = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"same"),
                "b.txt",
                "home",
                "",
                "operator",
            )
            .unwrap();
        assert_eq!(a.id, b.id, "same bytes same scope dedupe to one row");
        let c = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"same"),
                "a.txt",
                "inst-1",
                "",
                "operator",
            )
            .unwrap();
        assert_ne!(a.id, c.id, "a different scope is a different row");
    }

    #[test]
    fn refuses_bad_magic_and_offlist() {
        let (dir, s) = store();
        // PDF/image claims are refused while no processing contract
        // exists — a magic prefix is not processing.
        let err = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"hello"),
                "fake.pdf",
                "home",
                "",
                "operator",
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("not available yet"), "{err}");
        // a zip named .png is refused the same way (no image processing)
        let err = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"PK\x03\x04rest"),
                "a.png",
                "home",
                "",
                "operator",
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("not available yet"), "{err}");
        // svg is text-ish by shape but off the allowlist
        let err = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"<svg></svg>"),
                "x.txt",
                "home",
                "",
                "operator",
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("sniff"), "{err}");
        // an unsupported extension refuses before sniffing
        let err = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"MZ"),
                "a.exe",
                "home",
                "",
                "operator",
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an attachable type"), "{err}");
    }

    #[test]
    fn refuses_oversize_and_empty() {
        let (dir, s) = store();
        let big = vec![b'x'; (CHAT_FILE_MAX_BYTES + 1) as usize];
        let err = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), &big),
                "big.txt",
                "home",
                "",
                "operator",
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("cap"), "{err}");
        let err = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b""),
                "e.txt",
                "home",
                "",
                "operator",
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn refuses_tmp_outside_staging() {
        let (dir, s) = store();
        let elsewhere = dir.path().join("elsewhere.txt");
        std::fs::write(&elsewhere, b"hi").unwrap();
        let err = s
            .chat_file_put(dir.path(), &elsewhere, "e.txt", "home", "", "operator")
            .unwrap_err()
            .to_string();
        assert!(err.contains("directly under"), "{err}");
    }

    /// A row retained by an earlier build (image/PDF kinds were accepted
    /// then) still reads metadata-only — historical rows are preserved,
    /// never deleted, and never fabricated into text.
    fn legacy_row(s: &Store, dir: &Path, name: &str, mime: &str, bytes: &[u8]) -> ChatFile {
        let sha = {
            use sha2::Digest;
            sha2::Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let blob_dir = dir.join(CHAT_FILES_DIR);
        std::fs::create_dir_all(&blob_dir).unwrap();
        std::fs::write(blob_dir.join(&sha), bytes).unwrap();
        let file = ChatFile {
            id: format!("chf-{}", uuid::Uuid::new_v4().simple()),
            sha256: sha,
            size: bytes.len() as u64,
            mime: mime.to_string(),
            name: name.to_string(),
            scope: "home".to_string(),
            context_id: String::new(),
            uploader: "operator".to_string(),
            created: 0.0,
        };
        s.write_tx(|tx| {
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
            Ok(())
        })
        .unwrap();
        file
    }

    #[test]
    fn image_read_is_metadata_only() {
        let (dir, s) = store();
        let png = legacy_row(
            &s,
            dir.path(),
            "p.png",
            "image/png",
            b"\x89PNG\r\n\x1a\nrest",
        );
        let out = s.chat_file_read(dir.path(), &png.id).unwrap();
        assert_eq!(out["extractable"], json!(false));
        assert_eq!(out["mime"], json!("image/png"));
        assert_eq!(out["text"], Value::Null);
    }

    #[test]
    fn a_legacy_pdf_read_is_metadata_only() {
        let (dir, s) = store();
        let pdf = legacy_row(
            &s,
            dir.path(),
            "d.pdf",
            "application/pdf",
            b"%PDF-1.1\n1 0 obj\n<<>>\nendobj\n",
        );
        let out = s.chat_file_read(dir.path(), &pdf.id).unwrap();
        assert_eq!(out["extractable"], json!(false));
        assert_eq!(out["text"], Value::Null);
    }

    #[test]
    fn unknown_id_and_altered_blob_refuse() {
        let (dir, s) = store();
        assert!(s.chat_file_read(dir.path(), "chf-nope").is_err());
        let f = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"data"),
                "d.txt",
                "home",
                "",
                "operator",
            )
            .unwrap();
        // Corrupt the blob after landing.
        std::fs::write(blob_path(dir.path(), &f.sha256), b"tampered").unwrap();
        let err = s.chat_file_read(dir.path(), &f.id).unwrap_err().to_string();
        assert!(err.contains("digest"), "{err}");
    }

    #[test]
    fn csv_and_markdown_are_text_kinds() {
        let (dir, s) = store();
        let csv = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"a,b\n1,2"),
                "rows.csv",
                "home",
                "",
                "operator",
            )
            .unwrap();
        assert_eq!(csv.mime, "text/csv");
        let md = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"# hi"),
                "n.md",
                "home",
                "",
                "operator",
            )
            .unwrap();
        assert_eq!(md.mime, "text/markdown");
        let out = s.chat_file_read(dir.path(), &csv.id).unwrap();
        assert_eq!(out["text"], json!("a,b\n1,2"));
    }

    #[test]
    fn names_are_sanitized() {
        let (dir, s) = store();
        let f = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"x"),
                "../evil/\u{1}\u{2}name.txt",
                "home",
                "",
                "operator",
            )
            .unwrap();
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
        let a = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"1"),
                "a.txt",
                "home",
                "",
                "operator",
            )
            .unwrap();
        let b = s
            .chat_file_put(
                dir.path(),
                &stage(dir.path(), b"2"),
                "b.txt",
                "home",
                "",
                "operator",
            )
            .unwrap();
        let got = s.chat_files_for(&[b.id.clone(), a.id.clone()]).unwrap();
        assert_eq!(got[0].id, b.id);
        assert_eq!(got[1].id, a.id);
        assert!(s.chat_files_for(&["chf-missing".into()]).is_err());
    }
}
