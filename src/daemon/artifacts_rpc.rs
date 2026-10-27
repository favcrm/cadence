//! CAD-1726: the `artifact_*` RPCs behind the agent canvas.
//!
//! - `artifact_put {kind, title, content, id?}` — the master holding its
//!   live assigned turn (resolved by the daemon from the proven caller,
//!   re-proved against the persisted turn: running state, token current).
//!   The conversation and its scope come from THAT turn, never from the
//!   request; a request field naming a scope, thread or message is
//!   refused whole. `content` is base64 and capped at 10 MiB before any
//!   byte is stored. Putting an existing `id` adds the next version, and
//!   only inside the artifact's own conversation. The operator and every
//!   other agent are refused.
//! - `artifact_read {id, version, conversation}` and
//!   `artifact_versions {id, conversation}` — the operator only (the same
//!   `proven_operator` proof the board relays under). The stored scope is
//!   re-proved against the conversation being viewed: a row of another
//!   conversation, workspace or scope refuses before any byte is read.
//!
//! The store is chosen from how the daemon knows it is hosted (a held
//! lease or the image-owned business-documents admission): hosted uses
//! the refusing AOS-346 stub and never writes workspace custody.

use base64::Engine;
use serde_json::Value;

use super::*;
use crate::store::{Artifact, ArtifactStore, HostedArtifacts, LocalArtifacts, PutArtifact};

impl Shared {
    fn with_artifact_store<T>(&self, f: impl FnOnce(&dyn ArtifactStore) -> Result<T>) -> Result<T> {
        if self.lease.is_some() || self.hosted_business_documents.is_some() {
            return f(&HostedArtifacts);
        }
        f(&LocalArtifacts {
            store: &self.store,
            state_dir: &self.state_dir,
        })
    }

    /// Re-prove that `scope`/`context` still bind `thread_id` to a live
    /// conversation of the master: Home must be a genuine unarchived Home
    /// conversation; an app scope must be that exact conversation of the
    /// installation, with its context still current.
    fn prove_artifact_scope(&self, scope: &str, context: &str, thread_id: &str) -> Result<()> {
        match store::ChatFile::parse_scope(scope)? {
            None => {
                self.prove_home_conversation(Some(thread_id))?;
            }
            Some((install, conversation)) => {
                if conversation != thread_id {
                    return Err(Error::rejected(
                        "artifact scope does not match its conversation",
                    ));
                }
                let app = self.scoped_binding(install, context)?;
                self.prove_scoped_conversation(&app, conversation)?;
            }
        }
        Ok(())
    }

    /// The master's live assigned turn: `(alias, message)`, proven from the
    /// connection and re-proved against the persisted turn (running state,
    /// token current). Every other caller is refused. `verb` and `what`
    /// only word the refusals (`artifact put` / `puts artifacts`).
    pub(super) fn master_live_turn(
        &self,
        verb: &str,
        what: &str,
        peer_pid: u32,
    ) -> Result<(String, String)> {
        let alias = match self.caller_identity(peer_pid) {
            Ok(Caller::Agent(v)) => v.agent.alias.clone(),
            Ok(Caller::NoAgentIdentity) => {
                return Err(Error::rejected(format!(
                    "{verb} is the agent's: it needs the master's live assigned turn"
                )))
            }
            Err(e) => {
                return Err(Error::rejected(format!(
                    "{verb} refused: caller identity underivable — {e}"
                )))
            }
        };
        if !crate::master::is_master(&alias) {
            return Err(Error::rejected(format!(
                "{verb} refused for agent '{alias}' — only the master {what}, \
                 holding its live turn token"
            )));
        }
        let message = self.store.active_message_id(&alias)?.ok_or_else(|| {
            Error::rejected(format!(
                "{verb} needs the master's active assigned turn — none is running"
            ))
        })?;
        let stored = self
            .store
            .message(&message)?
            .ok_or_else(|| Error::rejected(format!("{verb}: turn is unknown")))?;
        let token = stored.turn_id.clone().unwrap_or_default();
        if token.is_empty() || stored.alias != alias || stored.state != "running" {
            return Err(Error::rejected(format!(
                "{verb} needs the master's active assigned turn"
            )));
        }
        let agent = self.store.agent(&alias)?;
        if !crate::adapter::registry::turn_token_current(
            &agent.provider,
            &agent.endpoint_kind,
            agent.generation.as_deref(),
            &token,
        ) {
            return Err(Error::rejected(format!(
                "{verb}: turn token is no longer current"
            )));
        }
        Ok((alias, message))
    }

    /// The scope of the master's live turn: `(scope, context, thread_id,
    /// alias, message)`, proven the way `chat_file_read` proves it.
    fn artifact_turn(&self, peer_pid: u32) -> Result<(String, String, String, String, String)> {
        let (alias, message) = self.master_live_turn("artifact put", "puts artifacts", peer_pid)?;
        let thread = self
            .store
            .message_conversation(&alias, &message)?
            .ok_or_else(|| {
                Error::rejected("artifact put: the turn's conversation cannot be resolved")
            })?;
        let (scope, context) = if thread.runs_as_home() {
            (store::CHAT_FILE_SCOPE_HOME.to_string(), String::new())
        } else {
            let install = thread.install_id.as_deref().unwrap_or_default();
            let stamp = self.store.message_app_stamp(&message)?.ok_or_else(|| {
                Error::rejected("artifact put: this turn carries no verified app binding")
            })?;
            if stamp.get("verified") != Some(&Value::Bool(true))
                || stamp.get("install_id").and_then(Value::as_str) != Some(install)
                || stamp
                    .get("conversation")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c != thread.id)
            {
                return Err(Error::rejected(
                    "artifact put: the turn's app stamp does not prove this conversation",
                ));
            }
            let context = match stamp
                .get("context_id")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
            {
                Some(stamped) => {
                    let proved = self.store.message_app(&message)?;
                    if proved.as_ref().and_then(|p| p["context_id"].as_str()) != Some(stamped) {
                        return Err(Error::rejected(
                            "artifact put: the turn's stamped context is no longer current",
                        ));
                    }
                    stamped.to_string()
                }
                None => String::new(),
            };
            (store::ChatFile::app_scope(install, &thread.id)?, context)
        };
        self.prove_artifact_scope(&scope, &context, &thread.id)?;
        Ok((scope, context, thread.id, alias, message))
    }

    pub(super) fn rpc_artifact_put(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_artifact_fields(params, &["kind", "title", "content", "id"])?;
        let (scope, context, thread_id, alias, message) = self.artifact_turn(peer_pid)?;
        let kind = required_str(params, "kind")?;
        let title = required_str(params, "title")?;
        let content = required_str(params, "content")?;
        // Refuse an oversize payload before decoding it (base64 is 4/3).
        if content.len() > store::ARTIFACT_MAX_BYTES / 3 * 4 + 8 {
            return Err(Error::rejected(format!(
                "artifact is over the {}-byte limit; nothing was stored",
                store::ARTIFACT_MAX_BYTES
            )));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|_| Error::rejected("artifact content must be base64"))?;
        let id = optional_str(params, "id");
        let artifact = self.with_artifact_store(|backend| {
            backend.put(
                PutArtifact {
                    id,
                    kind,
                    title,
                    thread_id: &thread_id,
                    scope: &scope,
                    context_id: &context,
                    created_by: &alias,
                    message_id: &message,
                },
                &bytes,
            )
        })?;
        self.changed.notify_all();
        Ok(artifact.to_json())
    }

    fn artifact_operator(&self, peer_pid: u32) -> Result<()> {
        match self.caller_identity(peer_pid) {
            Ok(Caller::NoAgentIdentity) => self.proven_operator("artifact read", peer_pid),
            Ok(Caller::Agent(_)) => Err(Error::rejected(
                "artifact read is the operator's — an agent may not read artifacts",
            )),
            Err(e) => Err(Error::rejected(format!(
                "artifact read refused: caller identity underivable — {e}"
            ))),
        }
    }

    /// The stored scope against the viewed conversation (the operator was
    /// proven first). Returns the row; bytes are read by the caller.
    fn artifact_in_view(
        &self,
        params: &Value,
        backend: &dyn ArtifactStore,
        version: i64,
    ) -> Result<Artifact> {
        let id = required_str(params, "id")?;
        if !store::artifact_id(id) {
            return Err(Error::rejected(
                "artifact id must be a daemon-minted `art-…` id",
            ));
        }
        let conversation = required_str(params, "conversation")?;
        proto::identifier(conversation, "conversation ID")?;
        let row = backend
            .meta(id, version)?
            .ok_or_else(|| Error::rejected("unknown artifact"))?;
        if row.thread_id != conversation {
            return Err(Error::rejected(
                "artifact does not belong to the conversation in view",
            ));
        }
        self.prove_artifact_scope(&row.scope, &row.context_id, &row.thread_id)?;
        Ok(row)
    }

    pub(super) fn rpc_artifact_read(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_artifact_fields(params, &["id", "version", "conversation"])?;
        self.artifact_operator(peer_pid)?;
        let version = optional_i64(params, "version")
            .filter(|v| *v >= 1)
            .ok_or_else(|| Error::rejected("artifact read needs a version"))?;
        self.with_artifact_store(|backend| {
            let row = self.artifact_in_view(params, backend, version)?;
            let bytes = backend.bytes(&row)?;
            let mime = row.mime(&bytes);
            let mut out = row.to_json();
            out["mime"] = json!(mime);
            out["content"] = json!(base64::engine::general_purpose::STANDARD.encode(&bytes));
            Ok(out)
        })
    }

    pub(super) fn rpc_artifact_versions(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_artifact_fields(params, &["id", "conversation"])?;
        self.artifact_operator(peer_pid)?;
        self.with_artifact_store(|backend| {
            // Prove against the newest version; every version of an id
            // shares one conversation and scope.
            let id = required_str(params, "id")?;
            let all = backend.versions(id)?;
            let newest = all
                .last()
                .ok_or_else(|| Error::rejected("unknown artifact"))?;
            self.artifact_in_view(params, backend, newest.version)?;
            Ok(json!({"id": id, "versions": all.iter().map(Artifact::to_json).collect::<Vec<_>>()}))
        })
    }

    /// Identity fields refuse, and so does every field outside the
    /// method's allowlist — a request never names scope, thread or turn.
    fn reject_artifact_fields(params: &Value, allowed: &[&str]) -> Result<()> {
        super::reject_identity_fields(params, "artifact")?;
        if let Some(obj) = params.as_object() {
            if let Some(field) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
                return Err(Error::rejected(format!(
                    "artifact field '{field}' is not accepted — scope and custody \
                     are the daemon's, never a request field"
                )));
            }
        }
        Ok(())
    }
}
