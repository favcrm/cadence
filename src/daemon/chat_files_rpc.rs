//! CAD-1168 slice 2: the `chat_file_*` RPCs behind the operator's
//! chat attachments.
//!
//! - `chat_file_upload` — OperatorOnly (the same `operator_chat` proof
//!   `thread_send` runs): a pane-attributed or underivable connection
//!   is refused before any byte is looked at. The staged tmp must live
//!   under `<state>/wiki-uploads/` (the shared upload staging dir the
//!   board writes — reused for staging only, never a wiki blob); the
//!   store re-hashes, caps, sniffs and lands it content-addressed.
//!   `{name, tmp}` alone is the Home shape, unchanged. An optional
//!   verified `app` (`{install_id, context_id?}`) plus the explicit
//!   `conversation` it is scoped to stores trusted provenance instead:
//!   the daemon proves the installation, the context and the master's
//!   unarchived conversation, re-proves the current exact-approved
//!   `file.upload` declaration of the live bundle, and encodes
//!   `app:<install>@<conversation>` in `scope` with the proven context
//!   in `context_id`. A caller-supplied scope, uploader, digest or path
//!   is refused whole; a conversation without an app binding refuses,
//!   and so does an app binding without its explicit conversation.
//! - `chat_file_read` — the operator (same proof) or the master holding
//!   a live assigned turn (resolved by the daemon from the proven
//!   caller; `message` + `token`, the pattern `app_run_artifact` uses,
//!   are optional and re-proved when sent) — and only for a file that same persisted
//!   turn envelope actually carries. A live token plus an id learned
//!   elsewhere (another turn's payload, an earlier message, an
//!   operator-side receipt) is refused before any byte is read. The
//!   stored provenance is re-proved before bytes are served: a master
//!   read against the turn's own `message_app` + `message_conversation`,
//!   an operator read against the stored conversation and the current
//!   approved declaration — the operator never bypasses scope merely by
//!   being the operator. Only text kinds are retained; a read returns
//!   bounded secret-scanned text.

use std::path::PathBuf;

use serde_json::Value;

use super::*;

impl Shared {
    /// The current exact-approved `file.upload` declaration for
    /// `install`: the descriptor-confined live bundle must be the digest
    /// the operator approved (CAD-1119 consent) and that bundle's
    /// manifest must declare the reserved tuple. The callback runs while
    /// the runtime snapshot is held, so the digest cannot change
    /// underneath it. An unknown install, an unapproved or revoked
    /// digest, a bundle that moved and a manifest without the
    /// declaration all refuse — a declaration is never a grant, and a
    /// stale catalog row is never authority. This is the one guard
    /// upload, reference and read share; no text-scope exemption and no
    /// browser projection substitutes for it.
    pub(super) fn with_file_upload_capability<T>(
        &self,
        install: &str,
        exclusive: bool,
        callback: impl FnOnce(&str) -> Result<T>,
    ) -> Result<T> {
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let store = &self.store;
        let check = |row: &Value, files: &std::collections::BTreeMap<String, String>| {
            let digest = row["digest"].as_str().ok_or_else(|| {
                Error::rejected("installation digest unavailable for the file-upload declaration")
            })?;
            if store.app_capability_status(install, digest)?["state"].as_str() != Some("approved") {
                return Err(Error::rejected(
                    "this installation's current bundle is not an approved runtime — the \
                     file-upload capability is not in force",
                ));
            }
            let manifest = crate::issue::app::parse_manifest(
                files
                    .get("app.md")
                    .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
            )?;
            if manifest.file_upload().is_none() {
                return Err(Error::rejected(
                    "this installation's approved bundle does not declare the file-upload \
                     capability",
                ));
            }
            callback(digest)
        };
        // An upload retains bytes and a row, so it holds the PM lock that
        // orders it against install, upgrade, remove and revoke; a read
        // writes nothing and takes the lock-free read (CAD-1189), so a
        // polling app turn never waits on the tracker write lock.
        if exclusive {
            crate::issue::app_catalog::workspace::with_runtime_snapshot(&pm, install, check)
        } else {
            crate::issue::app_catalog::workspace::with_runtime_read(&pm, install, check)
        }
    }

    /// The one native conversation/context proof a scoped operation
    /// needs, run while the approved runtime snapshot is held: the
    /// conversation exists, is the master's, is unarchived and belongs
    /// to this installation; a nonempty context must be an active
    /// stored context of the installation. An absent context stays
    /// absent — it is never filled from the conversation's creation
    /// context or any current/default context. A stale or mismatched
    /// conversation or context refuses rather than serving under a
    /// binding that no longer holds.
    pub(super) fn prove_scoped_conversation(&self, app: &Value, conversation: &str) -> Result<()> {
        // Re-run the native selector proof while the approved runtime snapshot
        // is held. Reconstruct only from the binding's install and explicitly
        // selected context; never carry normalized proof metadata across the
        // boundary or infer a context when it was absent.
        let mut selector = json!({"install_id": app["install_id"]});
        if let Some(context) = app.get("context_id") {
            selector["context_id"] = context.clone();
        }
        let fresh_app = super::thread_app(&selector, &self.store)?;
        self.verify_conversation_selector(crate::master::ALIAS, &fresh_app, conversation)
    }

    /// Reconstruct and prove the native binding used by scoped reads and
    /// references. An installation-only selector is checked against the
    /// catalog before entering the runtime snapshot; a supplied context is
    /// normalized by thread_app against the store. This never fills absent
    /// context from the conversation's creation context.
    pub(super) fn scoped_binding(&self, install: &str, context: &str) -> Result<Value> {
        let mut app = json!({"install_id": install});
        if !context.is_empty() {
            app["context_id"] = json!(context);
        }
        let app = super::thread_app(&app, &self.store)?;
        if app.get("context_id").is_none() {
            self.known_install(install)?;
        }
        Ok(app)
    }

    /// The reusable scoped-file guard (CAD-1168/CAD-1114 subset): hold
    /// the current exact-approved `file.upload` declaration open, prove
    /// the conversation and context natively, resolve `id` to a row
    /// whose stored provenance is exactly this installation, context
    /// and conversation, and only then read the exact checked bytes.
    /// Metadata and scope are proved before any byte is opened; no
    /// caller ever receives a path, and the bytes returned are the
    /// bytes whose size and digest were verified inside the held
    /// operation, so a completed proof is never undone by a second
    /// read. `cap` is clamped to the custody maximum; a row larger
    /// than `cap` refuses rather than truncating, so a caller needing a
    /// smaller window (the CRM CSV import's 256 KiB) never mistakes a
    /// truncated read for the full source.
    pub(super) fn scoped_chat_file(
        &self,
        install: &str,
        context: &str,
        conversation: &str,
        id: &str,
        cap: u64,
    ) -> Result<(store::ChatFile, Vec<u8>)> {
        if !store::chat_file_id(id) {
            return Err(Error::rejected(
                "scoped file needs a daemon-minted `chf-…` attachment id",
            ));
        }
        let app = self.scoped_binding(install, context)?;
        self.with_file_upload_capability(install, false, |_digest| {
            self.prove_scoped_conversation(&app, conversation)?;
            let file = self
                .store
                .chat_file(id)?
                .ok_or_else(|| Error::rejected(format!("unknown attachment '{id}'")))?;
            file.scope_matches(install, context, conversation)?;
            let bytes =
                self.store
                    .chat_file_ready_checked_in_workspace(&self.pm_dir()?, &file, cap)?;
            Ok((file, bytes))
        })
    }

    /// The same held guard for a whole `thread_send` attachment set:
    /// every row's stored provenance must be exactly this installation,
    /// context and conversation, and every row's bytes must pass the
    /// readiness checks — all inside the one approved snapshot, with
    /// the 50 MiB aggregate bounded by checked arithmetic. Returns the
    /// checked metadata rows (the bytes were verified and dropped);
    /// the reference path needs readiness, never the bytes.
    pub(super) fn scoped_chat_files_checked(
        &self,
        install: &str,
        context: &str,
        conversation: &str,
        ids: &[String],
    ) -> Result<Vec<store::ChatFile>> {
        let app = self.scoped_binding(install, context)?;
        self.with_file_upload_capability(install, false, |_digest| {
            self.prove_scoped_conversation(&app, conversation)?;
            let mut files = Vec::with_capacity(ids.len());
            let mut total: u64 = 0;
            for id in ids {
                let file = self
                    .store
                    .chat_file(id)?
                    .ok_or_else(|| Error::rejected(format!("unknown attachment '{id}'")))?;
                file.scope_matches(install, context, conversation)?;
                self.store.chat_file_ready_checked_in_workspace(
                    &self.pm_dir()?,
                    &file,
                    store::CHAT_FILE_MAX_BYTES,
                )?;
                total = total
                    .checked_add(file.size)
                    .ok_or_else(|| Error::rejected("attachment sizes overflow"))?;
                files.push(file);
            }
            if total > store::CHAT_FILE_MAX_PER_MESSAGE as u64 * store::CHAT_FILE_MAX_BYTES {
                return Err(Error::rejected(format!(
                    "attachments total {total} bytes — over the {}-byte message cap",
                    store::CHAT_FILE_MAX_PER_MESSAGE as u64 * store::CHAT_FILE_MAX_BYTES
                )));
            }
            Ok(files)
        })
    }

    /// `chat_file_upload {name, tmp, app?, conversation?}` —
    /// operator-proved; the daemon mints the id and derives the stored
    /// metadata from the real bytes, never from the request (name is a
    /// display hint the store sanitizes). Without an app the scope is
    /// the legacy Home label. With an app the binding is the daemon's
    /// (`thread_app` + `known_install` + `verify_conversation_selector`)
    /// and the current exact-approved `file.upload` declaration must
    /// hold before a byte is retained; the scope and context come from
    /// that proof, never from a caller field.
    pub(super) fn rpc_chat_file_upload(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_chat_file_fields(params, &["name", "tmp", "app", "conversation"])?;
        self.operator_chat("chat file upload", peer_pid)?;
        let name = required_str(params, "name")?;
        let tmp = PathBuf::from(required_str(params, "tmp")?);
        let app = match params.get("app") {
            None => None,
            Some(Value::Null) => {
                return Err(Error::rejected(
                    "app selector must be omitted or a verified binding — null is not accepted",
                ));
            }
            Some(v) => Some(super::thread_app(v, &self.store)?),
        };
        let conversation = match params.get("conversation") {
            None => None,
            Some(Value::Null) => {
                return Err(Error::rejected(
                    "conversation selector must be omitted or a string id — null is not accepted",
                ));
            }
            Some(Value::String(id)) => {
                proto::identifier(id, "conversation ID")?;
                Some(id.as_str())
            }
            Some(_) => return Err(Error::rejected("conversation must be a string id")),
        };
        let workspace_dir = self.pm_dir()?;
        let file = match (app.as_ref(), conversation) {
            (None, None) => self.store.chat_file_put_in_workspace(
                store::ChatFileStorageRoots {
                    state_dir: &self.state_dir,
                    workspace_dir: &workspace_dir,
                },
                &tmp,
                name,
                store::CHAT_FILE_SCOPE_HOME,
                "",
                "operator",
            )?,
            (None, Some(_)) => {
                return Err(Error::rejected(
                    "conversation needs a verified app binding — a home upload is scoped \
                     to the operator's own chat",
                ));
            }
            (Some(_), None) => {
                return Err(Error::rejected(
                    "an app upload needs the explicit conversation it is scoped to",
                ));
            }
            (Some(app), Some(conversation)) => {
                let install = app["install_id"].as_str().unwrap_or_default();
                // An installation-only binding (no context selected) is
                // proven by the catalog, exactly as `thread_send` does;
                // a context, when present, was proven by `thread_app`.
                if app.get("context_id").is_none() {
                    self.known_install(install)?;
                }
                // Context absence stays absent (installation-only
                // binding); it is never filled from the conversation's
                // creation context or any current/default context.
                let context = app["context_id"].as_str().unwrap_or("").to_string();
                let scope = store::ChatFile::app_scope(install, conversation)?;
                // The declaration and the conversation/context selector
                // are proved once, against the native seams, while the
                // exact-approved runtime snapshot is held: the actual
                // retention runs inside that held authorized operation,
                // not merely beside its label. An undeclared, revoked or
                // upgraded bundle, or a conversation/context that moved,
                // refuses the upload whole.
                self.with_file_upload_capability(install, true, |_digest| {
                    self.prove_scoped_conversation(app, conversation)?;
                    self.store.chat_file_put_in_workspace(
                        store::ChatFileStorageRoots {
                            state_dir: &self.state_dir,
                            workspace_dir: &workspace_dir,
                        },
                        &tmp,
                        name,
                        &scope,
                        &context,
                        "operator",
                    )
                })?
            }
        };
        Ok(file.to_json())
    }

    /// `chat_file_read {id}` for the operator, or for the master's live
    /// assigned turn — `{id}` alone resolves the master's own running
    /// turn from the proven caller (an app turn may not call
    /// `agent_show`); `{id, message, token}` is the explicit form
    /// `app_run_artifact` redeems. Narrowed to the master (the only
    /// agent chat attachments are ever delivered to); any other agent
    /// caller is refused. The stored provenance is re-proved before any
    /// byte is served.
    pub(super) fn rpc_chat_file_read(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_chat_file_fields(params, &["id", "message", "token"])?;
        let id = required_str(params, "id")?;
        if !store::chat_file_id(id) {
            return Err(Error::rejected(
                "chat file read needs a daemon-minted `chf-…` attachment id",
            ));
        }
        let out = match self.caller_identity(peer_pid) {
            Ok(Caller::NoAgentIdentity) => {
                self.proven_operator("chat file read", peer_pid)?;
                // The operator re-proves the stored provenance too: a
                // well-formed app row needs its live master
                // conversation, its current active stored context and
                // the current approved declaration, a Home row must be
                // a genuine Home row, and any other stored label
                // refuses. The checked bytes are projected directly —
                // the blob is never re-opened after a completed proof.
                let file = self
                    .store
                    .chat_file(id)?
                    .ok_or_else(|| Error::rejected(format!("unknown attachment '{id}'")))?;
                match store::ChatFile::parse_scope(&file.scope)? {
                    None => {
                        file.home_scope()?;
                        self.store
                            .chat_file_read_row_in_workspace(&self.pm_dir()?, &file)?
                    }
                    Some((install, conversation)) => {
                        let (file, bytes) = self.scoped_chat_file(
                            install,
                            &file.context_id,
                            conversation,
                            id,
                            store::CHAT_FILE_MAX_BYTES,
                        )?;
                        file.read_projection(bytes)?
                    }
                }
            }
            Ok(Caller::Agent(v)) => {
                let alias = v.agent.alias.clone();
                if !crate::master::is_master(&alias) {
                    return Err(Error::rejected(format!(
                        "chat file read refused for agent '{alias}' — only the master \
                         reads attachments, holding its live turn token",
                    )));
                }
                // The master's live assigned turn: the message addresses
                // it, is running under exactly this token, and the token
                // is current under the endpoint's own scheme — the
                // `scoped_chat_assistant` turn checks mirrored here.
                // A caller that names neither is resolved to the master's
                // own live message here (the pane needs no `agent_show`,
                // which an app turn may not call); naming only one is an
                // error. Either way the same checks below run.
                let (message, token) = match (params.get("message"), params.get("token")) {
                    (None, None) => {
                        let live = self.store.active_message_id(&alias)?.ok_or_else(|| {
                            Error::rejected(
                                "chat file read needs the master's active assigned turn — \
                                 none is running",
                            )
                        })?;
                        let turn = self
                            .store
                            .message(&live)?
                            .and_then(|m| m.turn_id)
                            .unwrap_or_default();
                        (live, turn)
                    }
                    _ => (
                        required_str(params, "message")?.to_string(),
                        required_str(params, "token")?.to_string(),
                    ),
                };
                let (message, token) = (message.as_str(), token.as_str());
                let stored = self
                    .store
                    .message(message)?
                    .ok_or_else(|| Error::rejected("chat file read: turn is unknown"))?;
                if token.is_empty()
                    || stored.alias != alias
                    || stored.state != "running"
                    || stored.turn_id.as_deref() != Some(token)
                {
                    return Err(Error::rejected(
                        "chat file read needs the master's active assigned turn",
                    ));
                }
                let agent = self.store.agent(&alias)?;
                if !crate::adapter::registry::turn_token_current(
                    &agent.provider,
                    &agent.endpoint_kind,
                    agent.generation.as_deref(),
                    token,
                ) {
                    return Err(Error::rejected(
                        "chat file read: turn token is no longer current",
                    ));
                }
                // The token proves the turn; it does not prove the
                // file. The id must occur in THAT message's persisted
                // attachment envelope — a retained id learned from
                // another turn, an earlier message or an operator
                // receipt is refused before a byte is served.
                let member = self
                    .store
                    .message_attachments(message)?
                    .as_ref()
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .any(|row| row.get("id").and_then(Value::as_str) == Some(id))
                    })
                    .unwrap_or(false);
                if !member {
                    return Err(Error::rejected(
                        "chat file read refused: this attachment is not on your turn's envelope",
                    ));
                }
                // The envelope proves membership; the stored provenance
                // must still match the turn's own conversation and
                // binding. The binding comes from the turn's own
                // persisted app stamp — the token proves the turn, the
                // stamp proves the binding — and a stamped context must
                // still be current. A stale context-bound turn refuses;
                // it is never read as an installation-only binding.
                let file = self
                    .store
                    .chat_file(id)?
                    .ok_or_else(|| Error::rejected(format!("unknown attachment '{id}'")))?;
                let thread = self
                    .store
                    .message_conversation(&alias, message)?
                    .ok_or_else(|| {
                        Error::rejected(
                            "chat file read: the turn's conversation cannot be resolved",
                        )
                    })?;
                if thread.is_home() {
                    file.home_scope()?;
                    self.store
                        .chat_file_read_row_in_workspace(&self.pm_dir()?, &file)?
                } else {
                    let install = thread.install_id.as_deref().unwrap_or_default();
                    let stamp = self.store.message_app_stamp(message)?.ok_or_else(|| {
                        Error::rejected("chat file read: this turn carries no verified app binding")
                    })?;
                    let stamp = stamp.as_object().ok_or_else(|| {
                        Error::rejected("chat file read: the turn's app stamp is malformed")
                    })?;
                    if stamp.get("verified") != Some(&Value::Bool(true))
                        || stamp.get("install_id").and_then(Value::as_str) != Some(install)
                    {
                        return Err(Error::rejected(
                            "chat file read: the turn's app stamp does not prove this \
                             installation",
                        ));
                    }
                    if let Some(selected) = stamp.get("conversation").and_then(Value::as_str) {
                        if selected != thread.id {
                            return Err(Error::rejected(
                                "chat file read: the turn's app stamp does not prove this \
                                 conversation",
                            ));
                        }
                    }
                    let context = match stamp
                        .get("context_id")
                        .and_then(Value::as_str)
                        .filter(|c| !c.is_empty())
                    {
                        Some(stamped) => {
                            let proved = self.store.message_app(message)?.ok_or_else(|| {
                                Error::rejected(
                                    "chat file read: the turn's stamped context is no \
                                     longer current",
                                )
                            })?;
                            if proved["context_id"].as_str() != Some(stamped) {
                                return Err(Error::rejected(
                                    "chat file read: the turn's stamped context is no \
                                     longer current",
                                ));
                            }
                            stamped.to_string()
                        }
                        None => String::new(),
                    };
                    // The same shared guard the upload and reference
                    // paths run: current exact-approved declaration,
                    // native conversation/context proof, exact stored
                    // provenance and the exact checked bytes.
                    let (file, bytes) = self.scoped_chat_file(
                        install,
                        &context,
                        &thread.id,
                        id,
                        store::CHAT_FILE_MAX_BYTES,
                    )?;
                    file.read_projection(bytes)?
                }
            }
            Err(e) => {
                return Err(Error::rejected(format!(
                    "chat file read refused: caller identity underivable — {e}"
                )));
            }
        };
        Ok(out)
    }

    /// `reject_identity_fields` plus the per-method allowlist — a
    /// caller-supplied `scope`, path or name-as-authority field refuses
    /// whole rather than being silently dropped.
    fn reject_chat_file_fields(params: &Value, allowed: &[&str]) -> Result<()> {
        super::reject_identity_fields(params, "chat file")?;
        if let Some(obj) = params.as_object() {
            if let Some(field) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
                return Err(Error::rejected(format!(
                    "chat file field '{field}' is not accepted — scope and custody \
                     are the daemon's, never a request field"
                )));
            }
        }
        Ok(())
    }
}
