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
//! The run/effect cross-check against app runs (proving the frozen caption
//! and asset are the reviewed ones) lands with the slice-3 E2E wiring; the
//! store already keeps `run_id`/`effect_id` for that join.

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
 receipt TEXT, created REAL NOT NULL, updated REAL NOT NULL);
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

/// Params for [`Store::social_publish_schedule`].
pub struct NewSocialPublish<'a> {
    pub request_id: &'a str,
    pub install_id: &'a str,
    pub context_id: Option<&'a str>,
    pub run_id: &'a str,
    pub effect_id: &'a str,
    pub connection_id: &'a str,
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
    if !device::valid_connection_id(row.connection_id) {
        return Err(bad("connection"));
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
        "run_id":row.run_id,"effect_id":row.effect_id,"connection_id":row.connection_id,
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
) -> Value {
    json!({"intent":{"schema":1,"intent_id":intent_id,"request":request,"state":state,
        "frozen":frozen,"frozen_digest":digest,"receipt":receipt}})
}

fn read_row(conn: &Connection, intent_id: &str) -> Result<Value> {
    let row: (String, String, String, String, String, Option<String>) = conn
        .query_row(
            "SELECT intent_id,request,state,frozen,frozen_digest,receipt FROM social_publish_intents WHERE intent_id=?",
            [intent_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?
        .ok_or_else(|| Error::rejected("social publish intent does not exist"))?;
    let frozen: Value = serde_json::from_str(&row.3)?;
    let receipt: Option<Value> = row
        .5
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| Error::rejected("social publish receipt is corrupt"))?;
    Ok(envelope(
        &row.0,
        &row.1,
        &row.2,
        &frozen,
        &row.4,
        receipt.as_ref(),
    ))
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
                "SELECT intent_id,request,state,frozen,frozen_digest,receipt FROM social_publish_intents WHERE request=?",
                [&request],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
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
            let receipt: Option<Value> = existing
                .5
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| Error::rejected("social publish receipt is corrupt"))?;
            tx.commit()?;
            return Ok(envelope(
                &existing.0,
                &existing.1,
                &existing.2,
                &frozen_value,
                &existing.4,
                receipt.as_ref(),
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

    /// Operator cancellation before dispatch. Any other state refuses.
    pub fn social_publish_cancel(&self, intent_id: &str) -> Result<Value> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let changed = tx.execute("UPDATE social_publish_intents SET state='cancelled',updated=? WHERE intent_id=? AND state='queued'",params![now(),intent_id])?;
        if changed != 1 {
            return Err(Error::rejected(
                "only a queued social publish intent can be cancelled",
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
        F: FnOnce(&Connection, &Value) -> Result<bool>,
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
        if !eligible(&tx, &frozen)? {
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

    /// Record the dispatch outcome. `posted` requires a verified
    /// permalink/receipt (a bare success string is refused); `refused`
    /// requires an error; `held` requires a reason and returns the intent
    /// to a human decision — it never silently republishes.
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
                    || receipt["provider_payload"].is_null()
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
