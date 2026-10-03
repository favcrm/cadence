//! CAD-771 slice 2: durable scheduled external-post intents.
//!
//! The operator-approved frozen intent for one exact-destination send lives
//! here: destination, digests, grant, approval identity, due time/timezone
//! and a stable idempotency key. States: queued -> processing ->
//! posted | refused, with cancelled (operator, before dispatch) and held
//! (authority expired at dispatch — a new human decision is required, never
//! a silent publish). A restart cannot lose or duplicate an intent: rows
//! are durable, dispatch claims atomically, and replays key off `request`.
//!
//! Shape validation reuses
//! [`crate::platform::agenticos_external::publish`], the read-only mirror
//! of the pinned AOS-94 device-publish v1 contract (PR #214 @ 12953144).
//! Freeze from an artifact re-proves the reviewed run material (caption and
//! asset digests derive from it) and, since CAD-1027, that the request's
//! install/context are the run's own and that `effect_id` is a live app
//! effect authorized by this run's artifact in that scope.

use super::*;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS social_publish_intents(
 intent_id TEXT PRIMARY KEY, request TEXT NOT NULL UNIQUE,
 install_id TEXT NOT NULL, context_id TEXT, run_id TEXT NOT NULL,
 effect_id TEXT NOT NULL, connection_id TEXT NOT NULL,
 destination_id TEXT NOT NULL, toolkit TEXT NOT NULL
   CHECK(toolkit IN ('instagram','facebook')),
 caption_digest TEXT NOT NULL, image_digest TEXT, media_key TEXT,
 grant_id TEXT NOT NULL, approval_id TEXT NOT NULL,
 due_epoch INTEGER NOT NULL CHECK(due_epoch>0), timezone TEXT NOT NULL,
 state TEXT NOT NULL
   CHECK(state IN ('queued','cancelled','processing','posted','refused','held')),
 frozen TEXT NOT NULL, frozen_digest TEXT NOT NULL,
 receipt TEXT, upstream TEXT, created REAL NOT NULL, updated REAL NOT NULL);
CREATE INDEX IF NOT EXISTS social_publish_due
 ON social_publish_intents(state,due_epoch,intent_id);
CREATE INDEX IF NOT EXISTS social_publish_install
 ON social_publish_intents(install_id,context_id,intent_id);
";

/// Event kinds on the platform stream for intent lifecycle.
pub const SOCIAL_PUBLISH_SCHEDULED_EVENT: &str = "social_publish_scheduled";
pub const SOCIAL_PUBLISH_CANCELLED_EVENT: &str = "social_publish_cancelled";
pub const SOCIAL_PUBLISH_CLAIMED_EVENT: &str = "social_publish_claimed";
pub const SOCIAL_PUBLISH_REPORTED_EVENT: &str = "social_publish_reported";

/// Params for [`Store::social_publish_schedule`]. The artifact triple
/// (`artifact_id`, `bundle_digest`, `slot`) is present exactly for
/// artifact-frozen intents; it lets dispatch re-prove the approved
/// material is unchanged instead of trusting a stale operator recheck.
#[allow(clippy::too_many_arguments)]
pub struct NewSocialPublish<'a> {
    pub request_id: &'a str,
    pub install_id: &'a str,
    pub context_id: Option<&'a str>,
    pub run_id: &'a str,
    pub effect_id: &'a str,
    pub artifact_id: Option<&'a str>,
    pub bundle_digest: Option<&'a str>,
    pub slot: Option<&'a str>,
    pub connection_id: &'a str,
    /// CAD-979 v9: the remote AOS `connectionId` — the upstream wire
    /// identity persisted into `frozen["aos_connection_id"]` and used by
    /// `SendBinding.connection_id`/`check_material`. The local
    /// `connection_id` stays the custody/install identity. Daemon-resolved;
    /// `src/store` only persists it (no network).
    pub aos_connection_id: Option<&'a str>,
    pub destination_id: &'a str,
    pub toolkit: &'a str,
    pub caption_digest: &'a str,
    pub image_digest: Option<&'a str>,
    pub media_key: Option<&'a str>,
    pub grant_id: &'a str,
    pub approval_id: &'a str,
    pub due_epoch: i64,
    pub timezone: &'a str,
}

fn validate_new(row: &NewSocialPublish<'_>) -> Result<()> {
    use crate::platform::agenticos_external::publish as device;
    let bad = |what: &str| Error::rejected(format!("social publish intent {what} is invalid"));
    if row.request_id.is_empty() || row.request_id.len() > 200 {
        return Err(bad("request id"));
    }
    if row.install_id.is_empty() || row.run_id.is_empty() || row.effect_id.is_empty() {
        return Err(bad("run identity"));
    }
    match (row.artifact_id, row.bundle_digest, row.slot) {
        (None, None, None) => {}
        (Some(artifact), Some(bundle), Some(slot))
            if !artifact.is_empty()
                && artifact.len() <= 200
                && !bundle.is_empty()
                && !slot.is_empty()
                && slot.len() <= 120 => {}
        _ => return Err(bad("artifact freeze triple")),
    }
    if !device::valid_connection_id(row.connection_id) {
        return Err(bad("connection"));
    }
    // v9: the AOS wire id, when present, must be a valid connection id too
    // — it is sent as `connectionId`/`grant.connectionId` on the wire.
    if let Some(aos) = row.aos_connection_id {
        if !device::valid_connection_id(aos) {
            return Err(bad("aos connection"));
        }
    }
    if row.destination_id.is_empty() || row.destination_id.len() > 120 {
        return Err(bad("destination"));
    }
    if device::Toolkit::parse(row.toolkit).is_none() {
        return Err(bad("toolkit"));
    }
    if !device::valid_digest(row.caption_digest) {
        return Err(bad("caption digest"));
    }
    match (row.toolkit, row.image_digest) {
        ("instagram", None) => return Err(bad("instagram needs an image digest")),
        (_, Some(digest)) if !device::valid_digest(digest) => {
            return Err(bad("image digest"));
        }
        _ => {}
    }
    if row
        .media_key
        .is_some_and(|key| !device::valid_media_key(key))
    {
        return Err(bad("media key"));
    }
    if !device::valid_grant_id(row.grant_id) {
        return Err(bad("grant"));
    }
    if row.approval_id.is_empty() || row.approval_id.len() > 120 {
        return Err(bad("approval"));
    }
    if row.due_epoch <= 0 {
        return Err(bad("due time"));
    }
    if row.timezone.is_empty()
        || row.timezone.len() > 64
        || !row
            .timezone
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
    {
        return Err(bad("timezone"));
    }
    Ok(())
}

fn frozen_of(row: &NewSocialPublish<'_>) -> Value {
    json!({"schema":1,"version":"1","install_id":row.install_id,"context_id":row.context_id,
        "run_id":row.run_id,"effect_id":row.effect_id,"artifact_id":row.artifact_id,
        "bundle_digest":row.bundle_digest,"slot":row.slot,
        "connection_id":row.connection_id,
        "aos_connection_id":row.aos_connection_id,
        "destination_id":row.destination_id,"toolkit":row.toolkit,
        "caption_digest":row.caption_digest,"image_digest":row.image_digest,
        "media_key":row.media_key,"grant_id":row.grant_id,"approval_id":row.approval_id,
        "due_epoch":row.due_epoch,"timezone":row.timezone})
}

fn envelope(
    intent_id: &str,
    request: &str,
    state: &str,
    frozen: &Value,
    digest: &str,
    receipt: Option<&Value>,
    upstream: Option<&Value>,
) -> Value {
    json!({"intent":{"schema":1,"intent_id":intent_id,"request":request,"state":state,
        "frozen":frozen,"frozen_digest":digest,"receipt":receipt,"upstream":upstream}})
}

fn parse_json_cell(cell: Option<String>, what: &str) -> Result<Option<Value>> {
    cell.as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| Error::rejected(format!("social publish {what} is corrupt")))
}

fn read_row(conn: &Connection, intent_id: &str) -> Result<Value> {
    let row: (
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
    ) = conn
        .query_row(
            "SELECT intent_id,request,state,frozen,frozen_digest,receipt,upstream,due_epoch FROM social_publish_intents WHERE intent_id=?",
            [intent_id],
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
        .optional()?
        .ok_or_else(|| Error::rejected("social publish intent does not exist"))?;
    let frozen: Value = serde_json::from_str(&row.3)?;
    let receipt = parse_json_cell(row.5, "receipt")?;
    let upstream = parse_json_cell(row.6, "upstream evidence")?;
    let mut env = envelope(
        &row.0,
        &row.1,
        &row.2,
        &frozen,
        &row.4,
        receipt.as_ref(),
        upstream.as_ref(),
    );
    // The `due_epoch` COLUMN is the scheduled-time source of truth the
    // claim SQL selects on; the send-now lateness check reads it so a
    // forged column can never hide behind the still-frozen
    // `frozen.due_epoch`.
    env["intent"]["due_epoch"] = json!(row.7);
    Ok(env)
}

impl Store {
    /// Durably store the frozen approved intent. Retries require the whole
    /// frozen binding, not merely matching provider arguments: the same
    /// `request` with different content fails instead of forking the key.
    pub fn social_publish_schedule(&self, row: &NewSocialPublish<'_>) -> Result<Value> {
        validate_new(row)?;
        let request = format!(
            "social-publish-{}",
            uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_OID,
                format!("{}:{}", row.install_id, row.request_id).as_bytes()
            )
            .simple()
        );
        crate::proto::identifier(row.request_id, "social publish request id")?;
        let frozen = frozen_of(row);
        let digest = app_runs::material_digest(&frozen);
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        if let Some(existing) = tx
            .query_row(
                "SELECT intent_id,request,state,frozen,frozen_digest,receipt,upstream FROM social_publish_intents WHERE request=?",
                [&request],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()?
        {
            if existing.4 != digest {
                return Err(Error::rejected(
                    "social publish request already names different frozen content",
                ));
            }
            let frozen_value: Value = serde_json::from_str(&existing.3)?;
            let receipt = parse_json_cell(existing.5, "receipt")?;
            let upstream = parse_json_cell(existing.6, "upstream evidence")?;
            tx.commit()?;
            return Ok(envelope(
                &existing.0,
                &existing.1,
                &existing.2,
                &frozen_value,
                &existing.4,
                receipt.as_ref(),
                upstream.as_ref(),
            ));
        }
        // CAD-1027: one operator approval authorizes exactly one intent. The
        // same-request retry returned above; the same approval under any
        // other request — a replay, a double submit with a fresh request id,
        // a re-schedule after cancel, another install — refuses. The check
        // and the insert share this transaction on the one write connection.
        let replayed: Option<String> = tx
            .query_row(
                "SELECT intent_id FROM social_publish_intents WHERE approval_id=?",
                [row.approval_id],
                |r| r.get(0),
            )
            .optional()?;
        if replayed.is_some() {
            return Err(Error::rejected(
                "approval_replay: this approval already authorized another social publish intent",
            ));
        }
        let intent_id = format!("spub-{}", uuid::Uuid::new_v4().simple());
        tx.execute("INSERT INTO social_publish_intents(intent_id,request,install_id,context_id,run_id,effect_id,connection_id,destination_id,toolkit,caption_digest,image_digest,media_key,grant_id,approval_id,due_epoch,timezone,state,frozen,frozen_digest,created,updated) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'queued',?,?,?,?)",
            params![intent_id,request,row.install_id,row.context_id,row.run_id,row.effect_id,row.connection_id,row.destination_id,row.toolkit,row.caption_digest,row.image_digest,row.media_key,row.grant_id,row.approval_id,row.due_epoch,row.timezone,frozen.to_string(),digest,now(),now()])?;
        Self::event(
            &tx,
            platform::PLATFORM_STREAM,
            SOCIAL_PUBLISH_SCHEDULED_EVENT,
            json!({"intent_id":intent_id,"request":request,"digest":digest}),
        )?;
        let result = read_row(&tx, &intent_id)?;
        tx.commit()?;
        Ok(result)
    }

    /// Operator cancellation before dispatch, scoped (CAD-1027): the intent
    /// must belong to exactly this install and context (null-preserving).
    /// Any other state or scope refuses and changes nothing.
    pub fn social_publish_cancel(
        &self,
        intent_id: &str,
        install_id: &str,
        context_id: Option<&str>,
    ) -> Result<Value> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let changed = tx.execute("UPDATE social_publish_intents SET state='cancelled',updated=? WHERE intent_id=? AND state='queued' AND install_id=? AND context_id IS ?",params![now(),intent_id,install_id,context_id])?;
        if changed != 1 {
            return Err(Error::rejected(
                "only a queued social publish intent in this install and context can be cancelled",
            ));
        }
        Self::event(
            &tx,
            platform::PLATFORM_STREAM,
            SOCIAL_PUBLISH_CANCELLED_EVENT,
            json!({"intent_id":intent_id}),
        )?;
        let result = read_row(&tx, intent_id)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn social_publish_show(&self, intent_id: &str) -> Result<Value> {
        read_row(&self.conn(), intent_id)
    }

    pub fn social_publish_list(
        &self,
        install: Option<&str>,
        context: Option<&str>,
    ) -> Result<Value> {
        if context.is_some() && install.is_none() {
            return Err(Error::rejected("context filter requires installation"));
        }
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT intent_id FROM social_publish_intents WHERE (? IS NULL OR install_id=?) AND (? IS NULL OR context_id=?) ORDER BY intent_id LIMIT 100")?;
        let ids = stmt
            .query_map(params![install, install, context, context], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut list = Vec::new();
        for id in ids {
            list.push(read_row(&conn, &id)?["intent"].clone());
        }
        Ok(json!({"intents":list}))
    }

    /// Peek the oldest due queued intent without claiming. The dispatch
    /// RPC uses it to compare operator-supplied current authority against
    /// frozen before claiming; the claim itself re-verifies in-transaction.
    pub(crate) fn social_publish_peek_due(&self, now_epoch: i64) -> Result<Option<Value>> {
        let conn = self.conn();
        let next: Option<String> = conn
            .query_row(
                "SELECT intent_id FROM social_publish_intents WHERE state='queued' AND due_epoch<=? ORDER BY due_epoch,intent_id LIMIT 1",
                [now_epoch],
                |r| r.get(0),
            )
            .optional()?;
        next.map(|id| read_row(&conn, &id)).transpose()
    }

    /// CAD-1020: one page of the driver's due rows — up to `limit` due
    /// queued intents, oldest first, after the `(due_epoch, intent_id)`
    /// keyset `after`. A read only; each row is still claimed by identity.
    pub(crate) fn social_publish_due_batch(
        &self,
        now_epoch: i64,
        limit: usize,
        after: Option<(i64, &str)>,
    ) -> Result<Vec<Value>> {
        let (after_due, after_id) = after.unwrap_or((i64::MIN, ""));
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT intent_id FROM social_publish_intents WHERE state='queued' AND due_epoch<=?1 AND (due_epoch>?3 OR (due_epoch=?3 AND intent_id>?4)) ORDER BY due_epoch,intent_id LIMIT ?2",
        )?;
        let ids = stmt
            .query_map(params![now_epoch, limit as i64, after_due, after_id], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        ids.iter().map(|id| read_row(&conn, id)).collect()
    }

    /// CAD-1020: `processing` rows for the driver's status reconcile, as
    /// `(intent_id, request, updated)`. `after` is a rotating cursor, so a
    /// bounded sweep reaches every row over successive ticks.
    pub(crate) fn social_publish_processing(
        &self,
        limit: usize,
        after: Option<&str>,
    ) -> Result<Vec<(String, String, f64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT intent_id,request,updated FROM social_publish_intents WHERE state='processing' AND (?1 IS NULL OR intent_id>?1) ORDER BY intent_id LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![after, limit.max(1) as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Re-prove approved material at dispatch for artifact-frozen intents.
    /// Reloads publication material (refuses stale binding or changed
    /// review itself) and requires artifact/asset digests to equal frozen.
    /// Any failure returns false: hold for a new human decision.
    /// Explicit-mode intents return true; operator recheck owns them.
    pub(crate) fn social_publish_material_current(&self, intent_id: &str) -> Result<bool> {
        let shown = read_row(&self.conn(), intent_id)?;
        let frozen = &shown["intent"]["frozen"];
        let artifact = frozen["artifact_id"].as_str();
        let bundle = frozen["bundle_digest"].as_str();
        let slot = frozen["slot"].as_str();
        if artifact.is_none() {
            return Ok(bundle.is_none() && slot.is_none());
        }
        let material = match self.app_publication_material(
            frozen["run_id"].as_str().unwrap_or(""),
            artifact.unwrap_or(""),
            bundle.unwrap_or(""),
            slot.unwrap_or(""),
        ) {
            Ok(material) => material,
            Err(_) => return Ok(false),
        };
        let caption = material["artifact"]["digest"].as_str().unwrap_or("");
        if caption.strip_prefix("sha256:").unwrap_or("")
            != frozen["caption_digest"].as_str().unwrap_or("")
        {
            return Ok(false);
        }
        let asset = material
            .get("asset")
            .and_then(|asset| asset["digest"].as_str())
            .and_then(|digest| digest.strip_prefix("sha256:"));
        Ok(asset == frozen["image_digest"].as_str())
    }

    /// Atomically claim the oldest due queued intent for dispatch. The
    /// caller proves current authority through `eligible` (grant, binding,
    /// app/context eligibility and unchanged digests at dispatch): a false
    /// verdict leaves the row queued. Exactly one claimant wins.
    pub(crate) fn social_publish_claim_due<F>(
        &self,
        now_epoch: i64,
        eligible: F,
    ) -> Result<Option<Value>>
    where
        F: FnOnce(&Connection, &str, &Value) -> Result<bool>,
    {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let next: Option<String> = tx
            .query_row(
                "SELECT intent_id FROM social_publish_intents WHERE state='queued' AND due_epoch<=? ORDER BY due_epoch,intent_id LIMIT 1",
                [now_epoch],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = next else {
            return Ok(None);
        };
        let frozen_text: String = tx.query_row(
            "SELECT frozen FROM social_publish_intents WHERE intent_id=?",
            [&id],
            |r| r.get(0),
        )?;
        let frozen: Value = serde_json::from_str(&frozen_text)?;
        // The candidate intent_id passes too: a caller that peeked one
        // id can pin its claim to that exact row — a queue-head move
        // between peek and claim is a no-claim, never a send of an
        // intent the caller never inspected.
        if !eligible(&tx, &id, &frozen)? {
            tx.commit()?;
            return Ok(None);
        }
        let changed = tx.execute("UPDATE social_publish_intents SET state='processing',updated=? WHERE intent_id=? AND state='queued'",params![now(),id])?;
        if changed != 1 {
            tx.commit()?;
            return Ok(None);
        }
        Self::event(
            &tx,
            platform::PLATFORM_STREAM,
            SOCIAL_PUBLISH_CLAIMED_EVENT,
            json!({"intent_id":id}),
        )?;
        let result = read_row(&tx, &id)?;
        tx.commit()?;
        Ok(Some(result))
    }

    /// CAD-1041: the operator's view of one intent in a named scope —
    /// the row only when it belongs to exactly this install and context
    /// (null-preserving, the CAD-1027 cancel predicate). Send-now reads
    /// through it so an out-of-scope request refuses before staging.
    pub(crate) fn social_publish_show_scoped(
        &self,
        intent_id: &str,
        install_id: &str,
        context_id: Option<&str>,
    ) -> Result<Value> {
        let conn = self.conn();
        let in_scope: Option<String> = conn
            .query_row(
                "SELECT intent_id FROM social_publish_intents WHERE intent_id=? AND install_id=? AND context_id IS ?",
                params![intent_id, install_id, context_id],
                |r| r.get(0),
            )
            .optional()?;
        if in_scope.is_none() {
            return Err(Error::rejected(
                "no social publish intent with this id in this install and context",
            ));
        }
        read_row(&conn, intent_id)
    }

    /// CAD-1041: atomically claim one NAMED queued intent in its own
    /// scope — the operator's send-now. Unlike `claim_due` there is no
    /// due_epoch filter: the operator's explicit click IS the dispatch
    /// trigger (the lateness bound runs earlier and refuses only the
    /// over-stale). The install and exact context are checked against the
    /// row inside the same compare-and-set as the state, so exactly one
    /// claimant wins and a wrong scope never claims; a second click, a
    /// racing `claim_due` or a cancel reads a non-queued row and yields
    /// `None`.
    pub(crate) fn social_publish_claim_id(
        &self,
        intent_id: &str,
        install_id: &str,
        context_id: Option<&str>,
    ) -> Result<Option<Value>> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE social_publish_intents SET state='processing',updated=? WHERE intent_id=? AND state='queued' AND install_id=? AND context_id IS ?",
            params![now(), intent_id, install_id, context_id],
        )?;
        if changed != 1 {
            tx.commit()?;
            return Ok(None);
        }
        Self::event(
            &tx,
            platform::PLATFORM_STREAM,
            SOCIAL_PUBLISH_CLAIMED_EVENT,
            json!({"intent_id":intent_id}),
        )?;
        let result = read_row(&tx, intent_id)?;
        tx.commit()?;
        Ok(Some(result))
    }

    /// Persist daemon-observed dispatch evidence on a processing intent.
    /// The evidence must carry the provider's exact binding plus its
    /// byte-exact payload as an opaque string (never re-serialized), AND
    /// its binding must equal the frozen intent: a status reply for the
    /// same key with a different destination/caption/image is refused
    /// here, never persisted. Refused while the intent is not processing.
    pub fn social_publish_note_evidence(&self, intent_id: &str, evidence: &Value) -> Result<Value> {
        for field in [
            "state",
            "permalink",
            "provider_ids",
            "provider_payload",
            "destination_id",
            "caption_digest",
        ] {
            if evidence.get(field).is_none() {
                return Err(Error::rejected(
                    "dispatch evidence is missing provider fields",
                ));
            }
        }
        if !matches!(
            evidence["state"].as_str(),
            Some("posted" | "processing" | "refused")
        ) || !evidence["provider_ids"].is_array()
            || !(evidence["provider_payload"].is_null()
                || evidence["provider_payload"]
                    .as_str()
                    .is_some_and(|payload| !payload.is_empty()))
        {
            return Err(Error::rejected(
                "dispatch evidence must carry state, ids and an opaque-or-absent payload",
            ));
        }
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let frozen_text: Option<String> = tx
            .query_row(
                "SELECT frozen FROM social_publish_intents WHERE intent_id=? AND state='processing'",
                [intent_id],
                |r| r.get(0),
            )
            .optional()?;
        let frozen_text = frozen_text
            .ok_or_else(|| Error::rejected("social publish intent is not processing"))?;
        let frozen: Value = serde_json::from_str(&frozen_text)?;
        let field = |doc: &Value, name: &str| doc.get(name).cloned().unwrap_or(Value::Null);
        if field(evidence, "destination_id") != field(&frozen, "destination_id")
            || field(evidence, "caption_digest") != field(&frozen, "caption_digest")
            || field(evidence, "image_digest") != field(&frozen, "image_digest")
        {
            return Err(Error::rejected(
                "dispatch evidence does not match the frozen intent",
            ));
        }
        let changed = tx.execute("UPDATE social_publish_intents SET upstream=?1,updated=?2 WHERE intent_id=?3 AND state='processing'",params![evidence.to_string(),now(),intent_id])?;
        if changed != 1 {
            return Err(Error::rejected("social publish intent is not processing"));
        }
        let result = read_row(&tx, intent_id)?;
        tx.commit()?;
        Ok(result)
    }

    /// Record the dispatch outcome. `posted` requires a verified
    /// permalink/receipt (a bare success string is refused) AND a receipt
    /// bound to the frozen intent: destination and content digests must
    /// equal frozen, or the report is refused and the intent stays
    /// processing (uncertain) — operator JSON alone never posts.
    /// `refused` requires an error; `held` requires a reason and returns
    /// the intent to a human decision — it never silently republishes.
    ///
    /// Processing-row recovery: a claimed intent whose dispatcher died
    /// survives restart as `processing` (durable row; re-claim finds
    /// nothing to claim, so no second send). Recover by reconciling the
    /// upstream status query for the intent's stable `request` key, then
    /// reporting the reconciled outcome here: posted with the reconciled
    /// binding+evidence, refused with the provider error, or held with
    /// the reason. Never re-execute to recover.
    pub fn social_publish_report(
        &self,
        intent_id: &str,
        decision: &str,
        receipt: &Value,
    ) -> Result<Value> {
        let state = match decision {
            "posted" => {
                if receipt["permalink"].as_str().is_none_or(str::is_empty)
                    || receipt["destination_id"].as_str().is_none_or(str::is_empty)
                    || receipt["caption_digest"].as_str().is_none_or(str::is_empty)
                    || !receipt["provider_ids"].is_array()
                    || receipt["provider_payload"]
                        .as_str()
                        .is_none_or(str::is_empty)
                {
                    return Err(Error::rejected(
                        "posted receipt needs permalink, binding and provider evidence",
                    ));
                }
                "posted"
            }
            "refused" => {
                if receipt["error"].as_str().is_none_or(str::is_empty) {
                    return Err(Error::rejected("refused receipt needs an error"));
                }
                "refused"
            }
            "held" => {
                if receipt["reason"].as_str().is_none_or(str::is_empty) {
                    return Err(Error::rejected("held receipt needs a reason"));
                }
                "held"
            }
            _ => {
                return Err(Error::rejected(
                    "social publish decision must be posted, refused or held",
                ));
            }
        };
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        if state == "posted" {
            let row: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT frozen,upstream FROM social_publish_intents WHERE intent_id=? AND state='processing'",
                    [intent_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (frozen_text, upstream_text) =
                row.ok_or_else(|| Error::rejected("social publish intent is not processing"))?;
            let frozen: Value = serde_json::from_str(&frozen_text)?;
            let receipt_digest = |field: &str| receipt.get(field).unwrap_or(&Value::Null);
            let frozen_digest = |field: &str| frozen.get(field).unwrap_or(&Value::Null);
            if receipt_digest("destination_id") != frozen_digest("destination_id")
                || receipt_digest("caption_digest") != frozen_digest("caption_digest")
                || receipt_digest("image_digest") != frozen_digest("image_digest")
            {
                return Err(Error::rejected(
                    "posted receipt does not match the frozen intent",
                ));
            }
            // Trusted upstream verification: the reported payload must be
            // byte-exact the bytes the daemon itself observed at dispatch.
            // A forged receipt with matching binding fields but fabricated
            // evidence fails closed here, and the intent stays processing.
            let upstream: Value = upstream_text
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| Error::rejected("social publish upstream evidence is corrupt"))?
                .ok_or_else(|| {
                    Error::rejected(
                        "no trusted upstream evidence; reconcile before reporting posted",
                    )
                })?;
            // Full receipt-to-outcome equality: payload-only comparison
            // lets a copied payload with forged permalink or IDs pass.
            // Every reported field must equal the daemon-observed outcome,
            // and the outcome's own binding must equal frozen (defense in
            // depth with the persistence-time check: no foreign evidence
            // can satisfy a posted report).
            let upstream_field = |name: &str| upstream.get(name).unwrap_or(&Value::Null);
            if upstream["state"] != "posted"
                || upstream_field("destination_id") != frozen_digest("destination_id")
                || upstream_field("caption_digest") != frozen_digest("caption_digest")
                || upstream_field("image_digest") != frozen_digest("image_digest")
                || receipt["permalink"] != upstream["permalink"]
                || receipt["provider_ids"] != upstream["provider_ids"]
                || receipt_digest("destination_id") != frozen_digest("destination_id")
                || receipt_digest("caption_digest") != frozen_digest("caption_digest")
                || receipt_digest("image_digest") != frozen_digest("image_digest")
                || receipt["provider_payload"].as_str() != upstream["provider_payload"].as_str()
                || upstream["provider_payload"]
                    .as_str()
                    .is_none_or(str::is_empty)
            {
                return Err(Error::rejected(
                    "posted receipt evidence does not match trusted upstream evidence",
                ));
            }
        }
        let changed = tx.execute("UPDATE social_publish_intents SET state=?1,receipt=?2,updated=?3 WHERE intent_id=?4 AND state='processing'",params![state,receipt.to_string(),now(),intent_id])?;
        if changed != 1 {
            return Err(Error::rejected("social publish intent is not processing"));
        }
        Self::event(
            &tx,
            platform::PLATFORM_STREAM,
            SOCIAL_PUBLISH_REPORTED_EVENT,
            json!({"intent_id":intent_id,"decision":decision}),
        )?;
        let result = read_row(&tx, intent_id)?;
        tx.commit()?;
        Ok(result)
    }
}

/// Strip the `sha256:` prefix Cadence artifact digests carry. The pinned
/// device contract speaks bare 64-hex; anything else is a malformed pin.
fn bare_digest(prefixed: &str) -> Result<&str> {
    use crate::platform::agenticos_external::publish as device;
    let hex = prefixed
        .strip_prefix("sha256:")
        .ok_or_else(|| Error::rejected("reviewed digest pin is malformed"))?;
    if !device::valid_digest(hex) {
        return Err(Error::rejected("reviewed digest pin is malformed"));
    }
    Ok(hex)
}

/// CAD-1027: the operator approval id the confirmation step mints —
/// `apv-` then exactly 32 lowercase hex (128 random bits).
pub fn valid_approval_id(raw: &str) -> bool {
    raw.strip_prefix("apv-").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Params for freezing an intent from reviewed run material instead of
/// caller-supplied digests.
#[allow(clippy::too_many_arguments)]
pub struct FreezeFromArtifact<'a> {
    pub request_id: &'a str,
    pub install_id: &'a str,
    pub context_id: Option<&'a str>,
    pub run_id: &'a str,
    pub artifact_id: &'a str,
    pub bundle_digest: &'a str,
    pub slot: &'a str,
    /// A live app effect authorized by this run's artifact in this exact
    /// install/context (CAD-1027): freeze refuses any other id.
    pub effect_id: &'a str,
    pub destination_id: &'a str,
    pub toolkit: &'a str,
    /// CAD-979 v9: the daemon-resolved remote AOS `connectionId` (wire
    /// identity). The `media_key` must bind THIS id — its `parts[2]` — not
    /// the local custody `connection_id`.
    pub aos_connection_id: &'a str,
    pub media_key: Option<&'a str>,
    pub grant_id: &'a str,
    pub approval_id: &'a str,
    pub due_epoch: i64,
    pub timezone: &'a str,
}

impl Store {
    /// Freeze from the approved run's reviewed material: the completed
    /// run, its independent review, the exact artifact bytes and the
    /// currently-current binding are all re-proven here (a later rebind,
    /// revoke or context change invalidates). Digests are derived, never
    /// trusted from the caller: the caption digest is the reviewed
    /// artifact's, the image digest the reviewed asset's (Instagram
    /// refuses without one; Facebook text-only proceeds without).
    pub fn social_publish_freeze_from_artifact(
        &self,
        row: &FreezeFromArtifact<'_>,
    ) -> Result<Value> {
        // CAD-1027: only the minted approval shape freezes, so the
        // per-confirmation, unguessable approval is not a UI-only property.
        if !valid_approval_id(row.approval_id) {
            return Err(Error::rejected(
                "bad_approval: approval id must be apv- followed by 32 lowercase hex",
            ));
        }
        let material = self.app_publication_material(
            row.run_id,
            row.artifact_id,
            row.bundle_digest,
            row.slot,
        )?;
        // CAD-1027: the request's install/context must be the run's own —
        // the material proves the run's binding in the run's scope only, so
        // a request naming another scope would otherwise freeze under it.
        // context_id is exact and null-preserving (no wildcard).
        if material["run"]["install_id"].as_str() != Some(row.install_id)
            || material["run"]["context_id"].as_str() != row.context_id
        {
            return Err(Error::rejected(
                "grant_binding_mismatch: schedule names a different install or context than the run",
            ));
        }
        // CAD-1027: the effect must be an app effect authorized by THIS run's
        // artifact in this exact scope — never a forged id or another run's.
        let effect_scope = self
            .conn()
            .query_row(
                // Live authority only: an app-artifact effect still waiting
                // or accepted. Declined/closed/executed effects never back
                // a post (the same live set authority changes close).
                "SELECT a.install_id,a.context_id,a.run_id,a.artifact_id FROM app_effect_authorizations a JOIN platform_effects e ON e.effect_id=a.effect_id WHERE a.effect_id=? AND e.authorization_kind='app_artifact' AND e.state IN ('waiting','decided')",
                [row.effect_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        if effect_scope
            .as_ref()
            .is_none_or(|(install, context, run, artifact)| {
                install != row.install_id
                    || context.as_deref() != row.context_id
                    || run != row.run_id
                    || artifact != row.artifact_id
            })
        {
            return Err(Error::rejected(
                "bad_effect: effect is not live authority for this run, artifact and scope",
            ));
        }
        let caption_digest = bare_digest(
            material["artifact"]["digest"]
                .as_str()
                .ok_or_else(|| Error::rejected("reviewed artifact digest is missing"))?,
        )?;
        let asset_digest = material
            .get("asset")
            .and_then(|asset| asset["digest"].as_str());
        let image_digest = match (row.toolkit, asset_digest) {
            ("instagram", Some(digest)) => Some(bare_digest(digest)?),
            ("instagram", None) => {
                return Err(Error::rejected("instagram needs a reviewed image asset"));
            }
            (_, digest) => digest.map(bare_digest).transpose()?,
        };
        let connection_id = material["binding"]["config"]["connection_id"]
            .as_str()
            .ok_or_else(|| Error::rejected("reviewed binding names no connection"))?;
        // CAD-979 (I4): a supplied `media_key` must bind THIS run's reviewed
        // asset — `dp1.<workspace>.<connection_id>.<image_digest[..32]>` — all
        // derived from the material, never the caller's word. A key for a
        // foreign connection or digest is refused here at freeze (send-time
        // `check_material` remains a second layer). The workspace comes from
        // the same frozen binding config.
        if let Some(key) = row.media_key {
            use crate::platform::agenticos_external::publish as device;
            // v9: the key's `parts[2]` is the remote AOS `connectionId` —
            // validate connection+digest against the resolved wire id, not
            // the local custody `connection_id`. The workspace (`parts[1]`)
            // is the send credential's upstream workspace which Cadence
            // never asserts locally — enforced upstream at mint/grant
            // (`SendGrant.authorize` `cross_workspace`), not here.
            let bound = image_digest.is_some_and(|digest| {
                device::media_key_authorizes_connection(key, row.aos_connection_id, digest)
            });
            if !bound {
                return Err(Error::rejected(
                    "grant_binding_mismatch: media key does not bind this connection and reviewed image",
                ));
            }
        }
        self.social_publish_schedule(&NewSocialPublish {
            request_id: row.request_id,
            install_id: row.install_id,
            context_id: row.context_id,
            run_id: row.run_id,
            effect_id: row.effect_id,
            artifact_id: Some(row.artifact_id),
            bundle_digest: Some(row.bundle_digest),
            slot: Some(row.slot),
            connection_id,
            aos_connection_id: Some(row.aos_connection_id),
            destination_id: row.destination_id,
            toolkit: row.toolkit,
            caption_digest,
            image_digest,
            media_key: row.media_key,
            grant_id: row.grant_id,
            approval_id: row.approval_id,
            due_epoch: row.due_epoch,
            timezone: row.timezone,
        })
    }
}
