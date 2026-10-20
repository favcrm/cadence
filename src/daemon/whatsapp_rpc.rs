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
/// CAD-1590: template-send, broadcast-test and broadcast records.
const EFFECTS_DIR: &str = "whatsapp-effects";
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
    effect_id_of(id, "wae_").map_err(|_| Error::rejected("WhatsApp reply effect id is invalid"))
}

/// CAD-1590: an effect id is `<prefix><32 hex>`; the prefix names its
/// kind (`wat_` template send, `wbt_` broadcast test, `wab_` broadcast
/// send/schedule), so an id never crosses verbs.
fn effect_id_of(id: &str, prefix: &str) -> Result<()> {
    let hex = id.strip_prefix(prefix).unwrap_or_default();
    if hex.len() != 32 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Error::rejected("WhatsApp effect id is invalid"));
    }
    Ok(())
}

/// CAD-1590: one template send, broadcast test or broadcast send. Same
/// states and claim as [`ReplyRecord`]. `facts` holds only door-sourced
/// ids, digests, counts and the masked test number — no message text
/// and no recipient phone number.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EffectRecord {
    schema: u32,
    effect_id: String,
    install_id: String,
    context_id: String,
    digest: String,
    state: String,
    code: Option<String>,
    updated: f64,
    broadcast_id: Option<String>,
    facts: Value,
}

fn prefix_of(method: &str) -> &'static str {
    match method {
        "whatsapp_template_send" => "wat_",
        "whatsapp_broadcast_test_send" => "wbt_",
        _ => "wab_",
    }
}

fn mint(prefix: &str) -> String {
    format!("{prefix}{}", uuid::Uuid::new_v4().simple())
}

/// A template variable or body parameter: 1–1024 UTF-16 units with no
/// surrounding whitespace (the contract refuses, never trims).
fn variable_value(value: &str, what: &str) -> Result<()> {
    let units = value.encode_utf16().count();
    if units == 0 || units > 1024 || value.trim() != value {
        return Err(Error::rejected(format!(
            "{what} must be 1-1024 characters without surrounding spaces"
        )));
    }
    Ok(())
}

/// `{"1": "...", ...}`: indexes 1–999, values per [`variable_value`].
fn template_variables(params: &Value) -> Result<std::collections::BTreeMap<u32, String>> {
    let object = params
        .get("variables")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::rejected("variables must be an object"))?;
    let mut out = std::collections::BTreeMap::new();
    for (key, value) in object {
        let index = key
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=999).contains(n) && key == &n.to_string())
            .ok_or_else(|| Error::rejected("template variable index is invalid"))?;
        let value = value
            .as_str()
            .ok_or_else(|| Error::rejected("template variable must be a string"))?;
        variable_value(value, "template variable")?;
        out.insert(index, value.to_string());
    }
    Ok(out)
}

/// Render `{{n}}` exactly as the contract's shared render does.
fn render_body(body: &str, variables: &std::collections::BTreeMap<u32, String>) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after[digits..].starts_with("}}") {
            let value = after[..digits]
                .parse::<u32>()
                .ok()
                .and_then(|n| variables.get(&n))
                .map(|v| v.trim())
                .filter(|v| !v.is_empty());
            match value {
                Some(value) => out.push_str(value),
                None => out.push_str(&rest[start..start + 4 + digits]),
            }
            rest = &after[digits + 2..];
        } else {
            out.push_str("{{");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// A broadcast draft body in the contract's shape. The door forces the
/// channel, connection and DRAFT; no status or schedule field exists here.
fn draft_body(params: &Value) -> Result<Value> {
    let ids = |value: &Value, max: usize, what: &str| -> Result<Vec<String>> {
        let list = value
            .as_array()
            .filter(|l| l.len() <= max)
            .ok_or_else(|| Error::rejected(format!("{what} must be a list of at most {max}")))?;
        list.iter()
            .map(|v| {
                let id = v.as_str().unwrap_or_default();
                door::check_path_id(id, what)?;
                Ok(id.to_string())
            })
            .collect()
    };
    let only = |value: &Value, allowed: &[&str], what: &str| -> Result<()> {
        let fields = value
            .as_object()
            .ok_or_else(|| Error::rejected(format!("{what} must be an object")))?;
        if fields.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(Error::rejected(format!("{what} has unsupported fields")));
        }
        Ok(())
    };
    let name = required_str(params, "name")?;
    if name.is_empty() || name.chars().count() > 200 {
        return Err(Error::rejected("broadcast name must be 1-200 characters"));
    }
    let template_id = required_str(params, "template_id")?;
    door::check_path_id(template_id, "template id")?;
    let parameters = params
        .get("body_parameters")
        .and_then(Value::as_array)
        .filter(|l| l.len() <= 20)
        .ok_or_else(|| Error::rejected("body_parameters must be a list of at most 20"))?;
    for value in parameters {
        variable_value(value.as_str().unwrap_or_default(), "body parameter")?;
    }
    let audience = params
        .get("audience")
        .ok_or_else(|| Error::rejected("audience is required"))?;
    let audience = match audience["source"].as_str() {
        Some("segment") => {
            only(audience, &["source", "segment_id"], "audience")?;
            let segment = required_str(audience, "segment_id")?;
            door::check_path_id(segment, "segment id")?;
            json!({"source": "segment", "segmentId": segment})
        }
        Some("individual") => {
            only(audience, &["source", "account_ids"], "audience")?;
            let accounts = ids(&audience["account_ids"], 1000, "account id")?;
            if accounts.is_empty() {
                return Err(Error::rejected("an individual audience needs an account"));
            }
            json!({"source": "individual", "accountIds": accounts})
        }
        _ => {
            return Err(Error::rejected(
                "audience source must be segment or individual",
            ))
        }
    };
    let mut body = json!({"name": name, "templateId": template_id,
        "bodyParameters": parameters, "audience": audience});
    if let Some(id) = optional_str(params, "broadcast_id")? {
        door::check_path_id(id, "broadcast id")?;
        body["broadcastId"] = json!(id);
    }
    if let Some(url) = optional_str(params, "header_media_url")? {
        if !url.starts_with("https://") || url.len() > 2048 || url.chars().any(char::is_control) {
            return Err(Error::rejected("header media URL must be https"));
        }
        body["headerMediaUrl"] = json!(url);
    }
    if let Some(exclusions) = params.get("exclusions") {
        only(exclusions, &["account_ids", "campaign_ids"], "exclusions")?;
        body["exclusions"] = json!({
            "accountIds": ids(&exclusions["account_ids"], 1000, "account id")?,
            "campaignIds": ids(&exclusions["campaign_ids"], 50, "campaign id")?,
        });
    }
    Ok(body)
}

/// Q8: `null` (now, through FavCRM's 60 s buffer) or RFC 3339 from
/// now + 5 min to now + 7 days.
fn send_at(params: &Value) -> Result<Option<String>> {
    let Some(raw) = optional_str(params, "send_at")? else {
        return Ok(None);
    };
    let at = chrono::DateTime::parse_from_rfc3339(raw)
        .map_err(|_| Error::rejected("send_at must be an RFC 3339 time"))?;
    let ahead = at.with_timezone(&chrono::Utc) - chrono::Utc::now();
    if ahead < chrono::Duration::minutes(5) || ahead > chrono::Duration::days(7) {
        return Err(Error::rejected("send_at must be 5 minutes to 7 days ahead"));
    }
    Ok(Some(
        at.with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    ))
}

fn expired(iso: &Value) -> bool {
    iso.as_str()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .is_none_or(|at| at.with_timezone(&chrono::Utc) <= chrono::Utc::now())
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
        // FavCRM message ids are UUIDs or `connector_<hex>` (never `=`/`+`),
        // so the path-token charset admits every real one (CAD-1588).
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
    fn whatsapp_dir(&self, name: &str) -> Result<std::path::PathBuf> {
        let dir = self.state_dir.join(name);
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::internal(format!("WhatsApp record store: {e}"))),
        }
        Ok(dir)
    }

    /// One record's bytes; `None` when it does not exist.
    fn whatsapp_record_read(&self, name: &str, id: &str) -> Result<Option<Vec<u8>>> {
        match std::fs::read(self.whatsapp_dir(name)?.join(format!("{id}.json"))) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::internal(format!("WhatsApp record store: {e}"))),
        }
    }

    /// Durable, private, atomic replace of one record.
    fn whatsapp_record_write(&self, name: &str, id: &str, bytes: &[u8]) -> Result<()> {
        let dir = self.whatsapp_dir(name)?;
        let path = dir.join(format!("{id}.json"));
        let tmp = dir.join(format!("{id}.json.tmp"));
        let _ = std::fs::remove_file(&tmp);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn whatsapp_reply_load(
        &self,
        effect_id: &str,
        install: &str,
        context: &str,
    ) -> Result<ReplyRecord> {
        effect_id_ok(effect_id)?;
        let unknown =
            || Error::rejected("WhatsApp reply is unknown in this installation and context");
        let Some(bytes) = self.whatsapp_record_read(REPLIES_DIR, effect_id)? else {
            return Err(unknown());
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
        self.whatsapp_record_write(REPLIES_DIR, &record.effect_id, &serde_json::to_vec(record)?)
    }

    fn whatsapp_effect_load(
        &self,
        effect_id: &str,
        prefix: &str,
        install: &str,
        context: &str,
    ) -> Result<EffectRecord> {
        effect_id_of(effect_id, prefix)?;
        let unknown =
            || Error::rejected("WhatsApp send is unknown in this installation and context");
        let Some(bytes) = self.whatsapp_record_read(EFFECTS_DIR, effect_id)? else {
            return Err(unknown());
        };
        let record: EffectRecord = serde_json::from_slice(&bytes)
            .map_err(|_| Error::internal("WhatsApp send record is unreadable"))?;
        if record.schema != 1
            || record.effect_id != effect_id
            || record.install_id != install
            || record.context_id != context
        {
            return Err(unknown());
        }
        Ok(record)
    }

    fn whatsapp_effect_save(&self, record: &EffectRecord) -> Result<()> {
        let prefix = record.effect_id.get(..4).unwrap_or_default();
        if !matches!(prefix, "wat_" | "wbt_" | "wab_") {
            return Err(Error::rejected("WhatsApp send effect id is invalid"));
        }
        effect_id_of(&record.effect_id, prefix)?;
        self.whatsapp_record_write(EFFECTS_DIR, &record.effect_id, &serde_json::to_vec(record)?)
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
        // The recipient and window the human approves come from the door's
        // live conversation, never from the frame. Only stable fields are
        // drawn, so a confirm refuses only if one of them changed.
        let conversation = self.whatsapp_door()?.conversation(&reply.conversation_id)?;
        if conversation["replyMode"] != "freeform" {
            return Err(Error::rejected(
                "the WhatsApp 24-hour window is closed; free text cannot be sent",
            ));
        }
        Ok(json!({
            "effect_id": effect_id,
            "key": door::reply_key(effect_id),
            "conversation_id": reply.conversation_id,
            "recipient": {
                "name": conversation["contact"]["name"],
                "phone": conversation["contact"]["phone"],
            },
            "reply_mode": conversation["replyMode"],
            "freeform_until": conversation["freeformUntil"],
            "text": reply.text,
            "reply_to_message_id": reply.reply_to_message_id,
            "digest": digest,
            "state": record.state,
        }))
    }

    /// CAD-1590: the SLOT card for `whatsapp.template.send`. The frame
    /// re-supplies the prepared template send; the host recomputes the
    /// digest over the live template's revision and content digest, so a
    /// changed, unapproved or altered template refuses. The recipient and
    /// connection come from the live conversation; the window is shown as
    /// information only — a template is allowed on a closed window.
    pub(super) fn whatsapp_template_facts(
        &self,
        install: &str,
        context: &str,
        args: &Value,
    ) -> Result<Value> {
        let effect_id = required_str(args, "effect_id")?;
        let record = self.whatsapp_effect_load(effect_id, "wat_", install, context)?;
        if !matches!(record.state.as_str(), "prepared" | "pending") {
            return Err(Error::rejected(
                "this WhatsApp template send is not awaiting approval",
            ));
        }
        let conversation_id = required_str(args, "conversation_id")?;
        let template_id = required_str(args, "template_id")?;
        let variables = template_variables(args)?;
        let door = self.whatsapp_door()?;
        let template = door.template(template_id)?;
        if template["status"] != "APPROVED" {
            return Err(Error::rejected(
                "this WhatsApp template is no longer approved",
            ));
        }
        let digest = door::template_digest(
            conversation_id,
            template_id,
            template["revision"].as_str().unwrap_or_default(),
            template["contentDigest"].as_str().unwrap_or_default(),
            &variables,
            effect_id,
        );
        if digest != record.digest {
            return Err(Error::rejected(
                "the template or its variables changed since it was prepared; prepare it again",
            ));
        }
        let conversation = door.conversation(conversation_id)?;
        Ok(json!({
            "effect_id": effect_id,
            "conversation_id": conversation_id,
            "recipient": {
                "name": conversation["contact"]["name"],
                "phone": conversation["contact"]["phone"],
            },
            "connection_id": template["connectionId"],
            "template": {
                "template_id": template_id,
                "name": template["name"],
                "language": template["language"],
                "revision": template["revision"],
            },
            "variables": variables,
            "rendered_body": render_body(template["bodyText"].as_str().unwrap_or_default(), &variables),
            "reply_mode": conversation["replyMode"],
            "digest": digest,
            "state": record.state,
        }))
    }

    /// CAD-1590: the SLOT card for `whatsapp.broadcast.test.send`: the
    /// door-masked number from the record, the live draft's template and
    /// the rendered sample. Only a DRAFT can send a test.
    pub(super) fn whatsapp_test_facts(
        &self,
        install: &str,
        context: &str,
        args: &Value,
    ) -> Result<Value> {
        let effect_id = required_str(args, "effect_id")?;
        let record = self.whatsapp_effect_load(effect_id, "wbt_", install, context)?;
        if !matches!(record.state.as_str(), "prepared" | "pending") {
            return Err(Error::rejected(
                "this WhatsApp test is not awaiting approval",
            ));
        }
        let broadcast_id = record.broadcast_id.as_deref().unwrap_or_default();
        let door = self.whatsapp_door()?;
        let shown = door.broadcast(broadcast_id)?;
        if shown["status"] != "DRAFT" {
            return Err(Error::rejected("only a draft broadcast can send a test"));
        }
        let estimate = door.broadcast_estimate(broadcast_id)?;
        Ok(json!({
            "effect_id": effect_id,
            "broadcast_id": broadcast_id,
            "name": shown["name"],
            "to": record.facts["to"],
            "template": shown["template"],
            "rendered_sample": estimate["preview"]["renderedBody"],
            "digest": record.digest,
            "state": record.state,
            "limit_note": "At most 5 test sends per broadcast per day.",
        }))
    }

    /// CAD-1590: the SLOT card for `whatsapp.broadcast.send`: FavCRM's
    /// frozen snapshot as the door answered it at prepare (connection,
    /// template and revision, audience, counts, send time), the live
    /// draft's variables cross-checked against it, and the stale bound.
    pub(super) fn whatsapp_broadcast_facts(
        &self,
        install: &str,
        context: &str,
        args: &Value,
    ) -> Result<Value> {
        let effect_id = required_str(args, "effect_id")?;
        let record = self.whatsapp_effect_load(effect_id, "wab_", install, context)?;
        if !matches!(record.state.as_str(), "prepared" | "pending") {
            return Err(Error::rejected(
                "this WhatsApp broadcast is not awaiting approval",
            ));
        }
        let snapshot = &record.facts;
        if expired(&snapshot["expires_at"]) {
            return Err(Error::rejected(
                "this broadcast review expired; prepare it again",
            ));
        }
        let broadcast_id = record.broadcast_id.as_deref().unwrap_or_default();
        let shown = self.whatsapp_door()?.broadcast(broadcast_id)?;
        if shown["status"] != "DRAFT"
            || shown["template"]["templateId"] != snapshot["template"]["templateId"]
            || shown["template"]["revision"] != snapshot["template"]["revision"]
        {
            return Err(Error::rejected(
                "the broadcast changed since it was reviewed; prepare it again",
            ));
        }
        let counts = &snapshot["counts"];
        let excluded = [
            "manuallyExcluded",
            "missingAddress",
            "missingConsent",
            "optedOut",
        ]
        .iter()
        .map(|k| counts[k].as_u64().unwrap_or_default())
        .sum::<u64>();
        Ok(json!({
            "effect_id": effect_id,
            "broadcast_id": broadcast_id,
            "name": shown["name"],
            "snapshot": snapshot,
            "body_parameters": shown["bodyParameters"],
            "header_media_url": shown["headerMediaUrl"],
            "eligible": counts["eligible"],
            "excluded": excluded,
            "send_at": snapshot["sendAt"],
            "digest": record.digest,
            "state": record.state,
        }))
    }

    /// CAD-1590: the SLOT card for `whatsapp.broadcast.stop`. Only stable
    /// fields are bound (they are recomputed at confirm), so a broadcast
    /// that keeps sending between card and tap can still be stopped.
    pub(super) fn whatsapp_stop_facts(&self, args: &Value) -> Result<Value> {
        let broadcast_id = required_str(args, "broadcast_id")?;
        let shown = self.whatsapp_door()?.broadcast(broadcast_id)?;
        if !matches!(
            shown["status"].as_str(),
            Some("SCHEDULED" | "SENDING" | "PARTIALLY_SENT")
        ) {
            return Err(Error::rejected("this broadcast has nothing to stop"));
        }
        Ok(json!({
            "broadcast_id": broadcast_id,
            "name": shown["name"],
            "template": shown["template"],
            "scheduled_at": shown["scheduledAt"],
        }))
    }

    /// CAD-1590: the stop card's live, unbound part — status and delivery
    /// counts as of drawing, approximate by the time of the tap.
    pub(super) fn whatsapp_stop_live(&self, args: &Value) -> Result<Value> {
        let shown = self
            .whatsapp_door()?
            .broadcast(required_str(args, "broadcast_id")?)?;
        Ok(json!({"status": shown["status"], "delivery_stats": shown["deliveryStats"]}))
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
            // CAD-1590: door v2, templates and broadcasts.
            "whatsapp_templates_list" => &["status"],
            "whatsapp_template_show" => &["template_id"],
            "whatsapp_template_prepare" => &["conversation_id", "template_id", "variables"],
            "whatsapp_template_send"
            | "whatsapp_broadcast_test_send"
            | "whatsapp_broadcast_send" => &["effect_id", "digest"],
            "whatsapp_template_status" => &["effect_id"],
            "whatsapp_broadcasts_list" => &["cursor", "limit"],
            "whatsapp_broadcast_show"
            | "whatsapp_broadcast_estimate"
            | "whatsapp_broadcast_stop" => &["broadcast_id"],
            "whatsapp_broadcast_results" => &["broadcast_id", "cursor", "limit", "status"],
            "whatsapp_broadcast_draft" => &[
                "broadcast_id",
                "name",
                "template_id",
                "body_parameters",
                "header_media_url",
                "audience",
                "exclusions",
            ],
            "whatsapp_broadcast_test_prepare" => &["broadcast_id", "to"],
            "whatsapp_broadcast_prepare" => &["broadcast_id", "send_at"],
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
                    // If this save fails the door keeps an orphan staged
                    // reply with no record. It is harmless: no host record
                    // means no send is ever claimed, and the door's stale
                    // bound expires it.
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
                _ => self.whatsapp_v2(door, method, params, install, context, cursor, limit),
            }
        })
    }

    /// CAD-1590: the door v2 verbs (`agenticos.whatsapp-door.v2`), inside
    /// the same fence as v1.
    #[allow(clippy::too_many_arguments)]
    fn whatsapp_v2(
        &self,
        door: &door::HostedWhatsapp,
        method: &str,
        params: &Value,
        install: &str,
        context: &str,
        cursor: Option<&str>,
        limit: Option<u64>,
    ) -> Result<Value> {
        let broadcast = || required_str(params, "broadcast_id");
        let record = |effect_id: &str, digest: String, broadcast_id: Option<&str>, facts: Value| {
            self.whatsapp_effect_save(&EffectRecord {
                schema: 1,
                effect_id: effect_id.to_string(),
                install_id: install.to_string(),
                context_id: context.to_string(),
                digest,
                state: "prepared".to_string(),
                code: None,
                updated: now_secs(),
                broadcast_id: broadcast_id.map(str::to_string),
                facts,
            })
        };
        match method {
            "whatsapp_templates_list" => door.templates(optional_str(params, "status")?),
            "whatsapp_template_show" => door.template(required_str(params, "template_id")?),
            "whatsapp_broadcasts_list" => door.broadcasts(cursor, limit),
            "whatsapp_broadcast_show" => door.broadcast(broadcast()?),
            "whatsapp_broadcast_estimate" => door.broadcast_estimate(broadcast()?),
            "whatsapp_broadcast_results" => {
                door.broadcast_results(broadcast()?, cursor, limit, optional_str(params, "status")?)
            }
            "whatsapp_broadcast_draft" => door.broadcast_draft(&mint("wad_"), &draft_body(params)?),
            "whatsapp_template_prepare" => {
                let effect_id = mint("wat_");
                let conversation_id = required_str(params, "conversation_id")?;
                let template_id = required_str(params, "template_id")?;
                let variables = template_variables(params)?;
                let (staged, digest) = door.template_prepare(
                    &effect_id,
                    conversation_id,
                    template_id,
                    &variables,
                    &effect_id,
                )?;
                record(&effect_id, digest.clone(), None, Value::Null)?;
                Ok(json!({
                    "effect_id": effect_id,
                    "key": effect_id,
                    "digest": digest,
                    "decision": staged["decision"],
                    "rendered_body": staged["renderedBody"],
                    "template_name": staged["templateName"],
                    "language": staged["language"],
                    "revision": staged["revision"],
                    "reply_mode": staged["replyMode"],
                    "expires_at": staged["expiresAt"],
                }))
            }
            "whatsapp_broadcast_test_prepare" => {
                let broadcast_id = broadcast()?;
                let to = required_str(params, "to")?;
                let digits = to.strip_prefix('+').unwrap_or_default();
                if !(8..=15).contains(&digits.len())
                    || digits.starts_with('0')
                    || !digits.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(Error::rejected(
                        "test number must be E.164, for example +85255550999",
                    ));
                }
                let effect_id = mint("wbt_");
                let staged =
                    door.broadcast_test_prepare(broadcast_id, &effect_id, to, &effect_id)?;
                let digest = staged["digest"].as_str().unwrap_or_default().to_string();
                // Only the door-masked number is kept.
                record(
                    &effect_id,
                    digest.clone(),
                    Some(broadcast_id),
                    json!({"to": staged["to"]}),
                )?;
                Ok(json!({
                    "effect_id": effect_id,
                    "key": effect_id,
                    "digest": digest,
                    "decision": staged["decision"],
                    "to": staged["to"],
                    "expires_at": staged["expiresAt"],
                }))
            }
            "whatsapp_broadcast_prepare" => {
                let broadcast_id = broadcast()?;
                let effect_id = mint("wab_");
                let at = send_at(params)?;
                let (staged, digest) =
                    door.broadcast_prepare(broadcast_id, &effect_id, at.as_deref(), &effect_id)?;
                let s = &staged["snapshot"];
                let count = |v: &Value| v.as_array().map_or(0, Vec::len);
                // Ids, digests and counts only: no variables, no phone numbers.
                let facts = json!({
                    "snapshotId": s["snapshotId"],
                    "snapshotDigest": s["snapshotDigest"],
                    "expires_at": staged["expiresAt"],
                    "connection": {"connectionId": s["connection"]["connectionId"]},
                    "template": s["template"],
                    "audience": {
                        "source": s["audience"]["source"],
                        "segmentId": s["audience"]["segmentId"],
                        "accounts": count(&s["audience"]["accountIds"]),
                        "excludedAccounts": count(&s["audience"]["exclusions"]["accountIds"]),
                        "excludedCampaigns": count(&s["audience"]["exclusions"]["campaignIds"]),
                    },
                    "counts": s["counts"],
                    "audienceDigest": s["audienceDigest"],
                    "sendAt": s["sendAt"],
                });
                record(&effect_id, digest.clone(), Some(broadcast_id), facts)?;
                Ok(json!({
                    "effect_id": effect_id,
                    "key": effect_id,
                    "digest": digest,
                    "decision": staged["decision"],
                    "expires_at": staged["expiresAt"],
                    "snapshot": staged["snapshot"],
                }))
            }
            "whatsapp_template_send"
            | "whatsapp_broadcast_test_send"
            | "whatsapp_broadcast_send" => {
                let effect_id = required_str(params, "effect_id")?;
                let digest = required_str(params, "digest")?;
                let mut record = {
                    let _claim = REPLY_CLAIM.lock().unwrap_or_else(|e| e.into_inner());
                    let mut record =
                        self.whatsapp_effect_load(effect_id, prefix_of(method), install, context)?;
                    if record.digest != digest {
                        return Err(Error::rejected(
                            "digest does not match the prepared WhatsApp send",
                        ));
                    }
                    if !matches!(record.state.as_str(), "prepared" | "pending") {
                        return Err(Error::rejected(format!(
                            "this WhatsApp send was already attempted ({}); it is never \
                             resent — read its status instead",
                            record.state
                        )));
                    }
                    // Durable before the door call: a crash leaves
                    // `sending`, which is never sent again.
                    record.state = "sending".to_string();
                    record.updated = now_secs();
                    self.whatsapp_effect_save(&record)?;
                    record
                };
                let broadcast_id = record.broadcast_id.clone().unwrap_or_default();
                let outcome = match method {
                    "whatsapp_template_send" => door.template_send(effect_id, digest),
                    "whatsapp_broadcast_test_send" => {
                        door.broadcast_test_send(&broadcast_id, effect_id, digest)
                    }
                    _ => door.broadcast_send(effect_id, digest),
                };
                record.state = outcome.state.as_str().to_string();
                record.code = outcome.code.clone();
                record.updated = now_secs();
                self.whatsapp_effect_save(&record)?;
                Ok(
                    json!({"effect_id": effect_id, "key": effect_id, "outcome": outcome.to_json(),
                    "delivery_claim": "favcrm-acceptance-only"}),
                )
            }
            "whatsapp_template_status" => {
                let effect_id = required_str(params, "effect_id")?;
                let record = self.whatsapp_effect_load(effect_id, "wat_", install, context)?;
                let recorded =
                    (record.state != "prepared").then(|| door.template_status(effect_id).to_json());
                Ok(
                    json!({"effect_id": effect_id, "key": effect_id, "state": record.state,
                    "code": record.code, "door": recorded}),
                )
            }
            "whatsapp_broadcast_stop" => door.broadcast_stop(broadcast()?, &mint("wbs_")),
            _ => Err(Error::rejected("unknown WhatsApp method")),
        }
    }
}

#[cfg(test)]
mod tests {
    //! CAD-1590 (Spec/security REVISE): the stop card binds only stable
    //! fields. The confirm step recomputes `whatsapp_stop_facts` and refuses
    //! on any difference, so a broadcast whose sent count rises between the
    //! card and the tap must yield identical facts (the stop is accepted),
    //! while the unbound live part reflects the new count.
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn cad1590_stop_facts_survive_a_rising_sent_count() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let sent = Arc::new(AtomicU64::new(3));
        let counter = sent.clone();
        std::thread::spawn(move || {
            for request in server.incoming_requests() {
                let now = counter.fetch_add(5, Ordering::SeqCst);
                let body = json!({"ok": true, "data": {
                    "contract": door::CONTRACT_V2, "broadcastId": "cmp_live", "name": "Autumn",
                    "status": "SENDING",
                    "template": {"templateId": "tpl_1", "name": "promo", "language": "en",
                        "revision": "2026-10-19T00:00:00.000Z"},
                    "scheduledAt": "2026-10-20T09:00:00.000Z", "deliveryStats": {"sent": now}}});
                let _ = request.respond(tiny_http::Response::from_string(body.to_string()));
            }
        });
        let root = tempfile::Builder::new().prefix("c1590s").tempdir().unwrap();
        let opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            hosted_whatsapp: Some(door::HostedWhatsapp::new(&base).unwrap()),
            ..Default::default()
        };
        let shared = Shared::new(root.path(), &opts).unwrap();
        let args = json!({"broadcast_id": "cmp_live"});
        let card = shared.whatsapp_stop_facts(&args).unwrap();
        let live_at_card = shared.whatsapp_stop_live(&args).unwrap();
        // Sending continues between drawing the card and the tap.
        let at_tap = shared.whatsapp_stop_facts(&args).unwrap();
        let live_at_tap = shared.whatsapp_stop_live(&args).unwrap();
        assert_eq!(card, at_tap, "stop facts must not move while sending");
        assert!(card.get("delivery_stats").is_none() && card.get("status").is_none());
        assert_ne!(live_at_card, live_at_tap, "live counts are shown, unbound");
        assert_eq!(card["template"]["revision"], "2026-10-19T00:00:00.000Z");
    }
}
