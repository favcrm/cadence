//! CAD-1562: the hosted WhatsApp door verbs (`agenticos.whatsapp-door.v1`).
//!
//! Every verb first proves the operator connection, refuses any field
//! outside its exact grammar, then proves the installation: its current
//! bundle approved, a screen package for this app declaring a
//! `whatsapp.*` operation, and the live context. Screen calls arrive
//! with a [`ScreenServiceProof`] and run inside the screen fence, which
//! additionally limits them to the operations the mounted package
//! declares (members and read-only mounts get the reads only).
//!
//! Calls go only through the hosted AgenticOS door; a self-hosted daemon
//! has none and refuses. The host never holds a FavCRM or Meta
//! credential, and never logs or stores the connect link.
//!
//! A reply is two steps. `whatsapp_reply_prepare` mints the effect id,
//! derives the door key from it, computes the digest, stages the reply at
//! the door and records `prepared`. `whatsapp_reply_send` (a host-drawn
//! SLOT on screens) executes it once: a record leaves `prepared` or
//! `pending` for `sending` before the door call, so a replayed or
//! concurrent send is refused here and never reaches the door. Every
//! other state is terminal; `uncertain` is reconciled only by the
//! read-only `whatsapp_reply_status`.
//!
//! Reply records are small private JSON files under
//! `<state>/whatsapp-replies/` (ids, digest and state; no message text).
//! No store schema changes, so an older binary simply ignores the
//! directory.
use super::app_screen_services_rpc::ScreenServiceProof;
use super::*;
use crate::issue::app_catalog::workspace;
use crate::platform::hosted_whatsapp::{self as door, Reply};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

const REPLIES_DIR: &str = "whatsapp-replies";
/// Serialises the prepared→sending claim within this daemon.
static REPLY_CLAIM: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyRecord {
    schema: u32,
    effect_id: String,
    install_id: String,
    context_id: String,
    digest: String,
    state: String,
    code: Option<String>,
    updated: f64,
}

fn effect_id_ok(id: &str) -> Result<()> {
    let hex = id.strip_prefix("wae_").unwrap_or_default();
    if hex.len() != 32 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Error::rejected("WhatsApp reply effect id is invalid"));
    }
    Ok(())
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default()
}

fn optional_str<'a>(params: &'a Value, field: &str) -> Result<Option<&'a str>> {
    match params.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(Error::rejected(format!("{field} must be a string"))),
    }
}

fn reply_from(params: &Value, effect_id: &str) -> Result<Reply> {
    let conversation_id = required_str(params, "conversation_id")?;
    door::check_conversation_id(conversation_id)?;
    let text = required_str(params, "text")?;
    // The contract bounds text in JavaScript string length (UTF-16 units).
    let units = text.encode_utf16().count();
    if units == 0 || units > door::TEXT_CHARS || text.trim().is_empty() {
        return Err(Error::rejected(
            "WhatsApp reply text must be 1-4096 characters",
        ));
    }
    let reply_to = optional_str(params, "reply_to_message_id")?;
    if let Some(id) = reply_to {
        door::check_conversation_id(id)
            .map_err(|_| Error::rejected("WhatsApp reply-to message id is invalid"))?;
    }
    Ok(Reply {
        conversation_id: conversation_id.to_string(),
        text: text.to_string(),
        effect_id: effect_id.to_string(),
        reply_to_message_id: reply_to.map(str::to_string),
    })
}

impl Shared {
    fn whatsapp_replies_dir(&self) -> Result<std::path::PathBuf> {
        let dir = self.state_dir.join(REPLIES_DIR);
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::internal(format!("WhatsApp reply store: {e}"))),
        }
        Ok(dir)
    }

    fn whatsapp_reply_load(
        &self,
        effect_id: &str,
        install: &str,
        context: &str,
    ) -> Result<ReplyRecord> {
        effect_id_ok(effect_id)?;
        let path = self
            .whatsapp_replies_dir()?
            .join(format!("{effect_id}.json"));
        let unknown =
            || Error::rejected("WhatsApp reply is unknown in this installation and context");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(unknown()),
            Err(e) => return Err(Error::internal(format!("WhatsApp reply store: {e}"))),
        };
        let record: ReplyRecord = serde_json::from_slice(&bytes)
            .map_err(|_| Error::internal("WhatsApp reply record is unreadable"))?;
        if record.schema != 1
            || record.effect_id != effect_id
            || record.install_id != install
            || record.context_id != context
        {
            return Err(unknown());
        }
        Ok(record)
    }

    fn whatsapp_reply_save(&self, record: &ReplyRecord) -> Result<()> {
        effect_id_ok(&record.effect_id)?;
        let dir = self.whatsapp_replies_dir()?;
        let path = dir.join(format!("{}.json", record.effect_id));
        let tmp = dir.join(format!("{}.json.tmp", record.effect_id));
        let _ = std::fs::remove_file(&tmp);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)?;
        file.write_all(&serde_json::to_vec(record)?)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// The SLOT card for `whatsapp.reply.send`: the reply the frame names,
    /// bound to the prepared record by the host-computed digest. Recomputed
    /// at confirm; any change refuses.
    pub(super) fn whatsapp_reply_facts(
        &self,
        install: &str,
        context: &str,
        args: &Value,
    ) -> Result<Value> {
        let effect_id = required_str(args, "effect_id")?;
        let record = self.whatsapp_reply_load(effect_id, install, context)?;
        let reply = reply_from(args, effect_id)?;
        let digest = door::reply_digest(&reply);
        if digest != record.digest {
            return Err(Error::rejected(
                "the reply differs from the prepared reply; prepare it again",
            ));
        }
        if !matches!(record.state.as_str(), "prepared" | "pending") {
            return Err(Error::rejected(
                "this WhatsApp reply is not awaiting approval",
            ));
        }
        Ok(json!({
            "effect_id": effect_id,
            "key": door::reply_key(effect_id),
            "conversation_id": reply.conversation_id,
            "text": reply.text,
            "reply_to_message_id": reply.reply_to_message_id,
            "digest": digest,
            "state": record.state,
        }))
    }

    /// Approval, declaration and live context for one installation. Screen
    /// calls use the screen fence, which proves all of this and more.
    fn whatsapp_scope<R>(
        self: &Arc<Self>,
        install: &str,
        context: &str,
        proof: Option<&ScreenServiceProof>,
        action: impl FnOnce() -> Result<R>,
    ) -> Result<R> {
        crate::proto::identifier(install, "installation ID")?;
        if proof.is_some() {
            return self.screen_service_scope(install, context, true, proof, action);
        }
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, install, |description, files| {
            let digest = required_str(description, "digest")?;
            if self.store.app_capability_status(install, digest)?["state"] != "approved" {
                return Err(Error::rejected("this app's current bundle is not approved"));
            }
            if !crate::issue::app_screen_pkg::declares_whatsapp_door(
                files,
                description["name"].as_str().unwrap_or_default(),
            ) {
                return Err(Error::rejected(
                    "this app does not declare WhatsApp messaging (whatsapp.*)",
                ));
            }
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.store.app_context_proof(install, context)?;
            Ok(())
        })?;
        action()
    }

    fn whatsapp_door(&self) -> Result<&door::HostedWhatsapp> {
        self.hosted_whatsapp.as_ref().ok_or_else(|| {
            Error::rejected(
                "WhatsApp messaging runs only on a hosted AgenticOS workspace; \
                 this self-hosted daemon has no AgenticOS WhatsApp door",
            )
        })
    }

    pub(super) fn rpc_whatsapp(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.rpc_whatsapp_with_proof(method, params, peer_pid, None)
    }

    pub(super) fn rpc_whatsapp_with_proof(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
        proof: Option<&ScreenServiceProof>,
    ) -> Result<Value> {
        Self::screen_service_method(proof, method)?;
        self.operator_connection("WhatsApp messaging", params, peer_pid)?;
        let extra: &[&str] = match method {
            "whatsapp_status" | "whatsapp_connect_session" => &[],
            "whatsapp_conversations_list" => &["cursor", "limit"],
            "whatsapp_conversations_show" => &["conversation_id"],
            "whatsapp_messages" => &["conversation_id", "cursor", "limit"],
            "whatsapp_reply_prepare" => &["conversation_id", "text", "reply_to_message_id"],
            "whatsapp_reply_send" => &["effect_id", "digest"],
            "whatsapp_reply_status" => &["effect_id"],
            _ => return Err(Error::rejected("unknown WhatsApp method")),
        };
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("WhatsApp payload must be an object"))?;
        if fields.keys().any(|key| {
            !matches!(key.as_str(), "install_id" | "context_id") && !extra.contains(&key.as_str())
        }) {
            return Err(Error::rejected("WhatsApp payload has unsupported fields"));
        }
        let install = required_str(params, "install_id")?;
        let context = required_str(params, "context_id")?;
        let cursor = optional_str(params, "cursor")?;
        let limit = match params.get("limit") {
            None => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .ok_or_else(|| Error::rejected("limit must be a positive integer"))?,
            ),
        };
        self.whatsapp_scope(install, context, proof, || {
            let door = self.whatsapp_door()?;
            match method {
                "whatsapp_status" => door.status(),
                "whatsapp_connect_session" => door.connect_session(),
                "whatsapp_conversations_list" => door.conversations(cursor, limit),
                "whatsapp_conversations_show" => {
                    door.conversation(required_str(params, "conversation_id")?)
                }
                "whatsapp_messages" => {
                    door.messages(required_str(params, "conversation_id")?, cursor, limit)
                }
                "whatsapp_reply_prepare" => {
                    let effect_id = format!("wae_{}", uuid::Uuid::new_v4().simple());
                    let reply = reply_from(params, &effect_id)?;
                    let digest = door::reply_digest(&reply);
                    let staged = door.reply_prepare(&reply)?;
                    self.whatsapp_reply_save(&ReplyRecord {
                        schema: 1,
                        effect_id: effect_id.clone(),
                        install_id: install.to_string(),
                        context_id: context.to_string(),
                        digest: digest.clone(),
                        state: "prepared".to_string(),
                        code: None,
                        updated: now_secs(),
                    })?;
                    Ok(json!({
                        "effect_id": effect_id,
                        "key": door::reply_key(&effect_id),
                        "digest": digest,
                        "decision": staged["decision"],
                        "reply_mode": staged["replyMode"],
                        "freeform_until": staged["freeformUntil"],
                        "expires_at": staged["expiresAt"],
                    }))
                }
                "whatsapp_reply_send" => {
                    let effect_id = required_str(params, "effect_id")?;
                    let digest = required_str(params, "digest")?;
                    let key = door::reply_key(effect_id);
                    let mut record = {
                        let _claim = REPLY_CLAIM.lock().unwrap_or_else(|e| e.into_inner());
                        let mut record = self.whatsapp_reply_load(effect_id, install, context)?;
                        if record.digest != digest {
                            return Err(Error::rejected(
                                "digest does not match the prepared WhatsApp reply",
                            ));
                        }
                        if !matches!(record.state.as_str(), "prepared" | "pending") {
                            return Err(Error::rejected(format!(
                                "this WhatsApp reply was already attempted ({}); it is never \
                                 resent — read its status instead",
                                record.state
                            )));
                        }
                        // Durable before the door call: a crash leaves
                        // `sending`, which is never sent again.
                        record.state = "sending".to_string();
                        record.updated = now_secs();
                        self.whatsapp_reply_save(&record)?;
                        record
                    };
                    let outcome = door.reply_send(&key, digest);
                    record.state = outcome.state.as_str().to_string();
                    record.code = outcome.code.clone();
                    record.updated = now_secs();
                    self.whatsapp_reply_save(&record)?;
                    Ok(
                        json!({"effect_id": effect_id, "key": key, "reply": outcome.to_json(),
                        "delivery_claim": "favcrm-acceptance-only"}),
                    )
                }
                "whatsapp_reply_status" => {
                    let effect_id = required_str(params, "effect_id")?;
                    let record = self.whatsapp_reply_load(effect_id, install, context)?;
                    let key = door::reply_key(effect_id);
                    let recorded =
                        (record.state != "prepared").then(|| door.reply_status(&key).to_json());
                    Ok(
                        json!({"effect_id": effect_id, "key": key, "state": record.state,
                        "code": record.code, "door": recorded}),
                    )
                }
                _ => Err(Error::rejected("unknown WhatsApp method")),
            }
        })
    }
}
