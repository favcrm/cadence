//! Approved bounded campaign sends inside the installation's own
//! record file (CAD-786).
//!
//! Every recipient byte — email, name-derived first name, the raw
//! unsubscribe token — stays inside this per-installation SQLite
//! file, the same PII custody `app_records` already holds. What
//! leaves the file is digests and counts only: core sees the
//! PII-free `crm_sends` intent plus a sha256 token index, and every
//! show/list masks addresses.
//!
//! `app_campaign_sends.state` is the approval state machine:
//! `prepared → sending → completed`, or `closed` with a reason when
//! the authority underneath (context, link, credential, content
//! approval, audience validity) is gone mid-send. Deliveries move
//! `queued → submitting → accepted | failed | uncertain |
//! suppressed | closed`; `uncertain` is operator-resolved only,
//! never retried.

use super::app_audiences::AudienceBase;
use super::app_records::{email_shape_valid, ConsentState, CustomerProfile, RecordStore};
use super::app_runs::material_digest;
use super::StoreConn;
use crate::error::{Error, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

/// Frozen prepared-send fields: everything `send_digest` commits.
pub struct SendDraft {
    pub send_id: String,
    pub campaign_id: String,
    pub request_id: String,
    pub content_revision: i64,
    pub content_digest: String,
    pub audience_freeze_id: String,
    pub audience_digest: String,
    pub connection_id: String,
    pub auth_revision: i64,
    pub link_revision: i64,
    pub link_digest: String,
    pub max_recipients: i64,
    /// The unsubscribe origin frozen into the digest — the links a
    /// later origin change would strand.
    pub unsubscribe_origin: String,
    pub send_digest: String,
}

/// One prepared/approved send row.
#[derive(Clone)]
pub struct CampaignSend {
    pub send_id: String,
    pub campaign_id: String,
    pub request_id: String,
    pub content_revision: i64,
    pub content_digest: String,
    pub audience_freeze_id: String,
    pub audience_digest: String,
    pub connection_id: String,
    pub auth_revision: i64,
    pub link_revision: i64,
    pub link_digest: String,
    pub max_recipients: i64,
    pub unsubscribe_origin: String,
    pub send_digest: String,
    pub state: String,
    pub close_reason: Option<String>,
    pub created: f64,
    pub approved_at: Option<f64>,
}

/// One per-recipient delivery row.
#[derive(Clone)]
pub struct Delivery {
    pub send_id: String,
    pub customer_id: String,
    pub email: String,
    pub idempotency_key: String,
    pub state: String,
    pub attempts: i64,
    pub smtp_code: Option<i64>,
    pub reason: Option<String>,
    pub resolved_by: Option<String>,
}

/// What `customer_sendable` decides for one frozen member.
pub enum Sendable {
    /// Email plus the name the `{{first_name|fallback}}` token fills
    /// with — `None` falls back to the block's own fallback text.
    Ok {
        email: String,
        first_name: Option<String>,
    },
    /// Consent is not `granted`, the address is invalid/absent, or a
    /// suppression row covers the customer or address.
    Refused(&'static str),
}

/// `idempotency_key = material_digest({domain:"cadence-crm-delivery-v1", …})`
/// — stable across crashes, so a re-materialization can never mint a
/// second row for the same recipient.
pub fn delivery_idempotency_key(
    install: &str,
    context: &str,
    send_id: &str,
    customer_id: &str,
) -> String {
    material_digest(&json!({
        "domain": "cadence-crm-delivery-v1",
        "install_id": install,
        "context_id": context,
        "send_id": send_id,
        "customer_id": customer_id,
    }))
}

/// `send_digest` commits every frozen field the operator approved.
pub fn send_digest(install: &str, context: &str, draft: &SendDraft) -> String {
    material_digest(&json!({
        "domain": "cadence-crm-send-v1",
        "install_id": install,
        "context_id": context,
        "send_id": draft.send_id,
        "campaign_id": draft.campaign_id,
        "request_id": draft.request_id,
        "content_revision": draft.content_revision,
        "content_digest": draft.content_digest,
        "audience_freeze_id": draft.audience_freeze_id,
        "audience_digest": draft.audience_digest,
        "connection_id": draft.connection_id,
        "auth_revision": draft.auth_revision,
        "link_revision": draft.link_revision,
        "link_digest": draft.link_digest,
        "max_recipients": draft.max_recipients,
        "unsubscribe_origin": draft.unsubscribe_origin,
    }))
}

/// sha256 hex of a raw unsubscribe token — the only form any index
/// stores. The token itself is a 32-byte credential; its hash names
/// the redemption target without being redeemable.
pub fn unsubscribe_token_hash(token: &str) -> String {
    crate::store::app_runs::artifact_digest(token.as_bytes())
}

/// 32 random bytes, base64url — one token per delivery row. The
/// recipient's URL carries it; every store keeps only its hash
/// (plus the raw token inside this PII-custody file so a resumed
/// worker can rebuild the exact URL it minted at approve time).
pub fn mint_unsubscribe_token() -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(&bytes)
}

/// `a***@example.com` — show/list never returns a full address.
pub fn masked_email(email: &str) -> String {
    match email.split_once('@') {
        Some((user, domain)) if !user.is_empty() => {
            format!("{}***@{domain}", &user[..1])
        }
        _ => "***".to_string(),
    }
}

fn row_send(row: &rusqlite::Row<'_>) -> rusqlite::Result<CampaignSend> {
    Ok(CampaignSend {
        send_id: row.get(0)?,
        campaign_id: row.get(1)?,
        request_id: row.get(2)?,
        content_revision: row.get(3)?,
        content_digest: row.get(4)?,
        audience_freeze_id: row.get(5)?,
        audience_digest: row.get(6)?,
        connection_id: row.get(7)?,
        auth_revision: row.get(8)?,
        link_revision: row.get(9)?,
        link_digest: row.get(10)?,
        max_recipients: row.get(11)?,
        send_digest: row.get(12)?,
        unsubscribe_origin: row.get(13)?,
        state: row.get(14)?,
        close_reason: row.get(15)?,
        created: row.get(16)?,
        approved_at: row.get(17)?,
    })
}

const SEND_COLUMNS: &str = "send_id,campaign_id,request_id,content_revision,content_digest,audience_freeze_id,audience_digest,connection_id,auth_revision,link_revision,link_digest,max_recipients,send_digest,unsubscribe_origin,state,close_reason,created,approved_at";

fn row_delivery(row: &rusqlite::Row<'_>) -> rusqlite::Result<Delivery> {
    Ok(Delivery {
        send_id: row.get(0)?,
        customer_id: row.get(1)?,
        email: row.get(2)?,
        idempotency_key: row.get(3)?,
        state: row.get(4)?,
        attempts: row.get(5)?,
        smtp_code: row.get(6)?,
        reason: row.get(7)?,
        resolved_by: row.get(8)?,
    })
}

const DELIVERY_COLUMNS: &str =
    "send_id,customer_id,email,idempotency_key,state,attempts,smtp_code,reason,resolved_by";

fn send_json(install: &str, context: &str, send: &CampaignSend) -> Value {
    json!({
        "send_id": send.send_id,
        "install_id": install,
        "context_id": context,
        "campaign_id": send.campaign_id,
        "request_id": send.request_id,
        "content_revision": send.content_revision,
        "content_digest": send.content_digest,
        "audience_freeze_id": send.audience_freeze_id,
        "audience_digest": send.audience_digest,
        "connection_id": send.connection_id,
        "auth_revision": send.auth_revision,
        "link_revision": send.link_revision,
        "link_digest": send.link_digest,
        "max_recipients": send.max_recipients,
        "send_digest": send.send_digest,
        "state": send.state,
        "close_reason": send.close_reason,
        "created": send.created,
        "approved_at": send.approved_at,
    })
}

fn delivery_json(delivery: &Delivery) -> Value {
    json!({
        "send_id": delivery.send_id,
        "customer_id": delivery.customer_id,
        "email": masked_email(&delivery.email),
        "idempotency_key": delivery.idempotency_key,
        "state": delivery.state,
        "attempts": delivery.attempts,
        "smtp_code": delivery.smtp_code,
        "reason": delivery.reason,
        "resolved_by": delivery.resolved_by,
        "delivery_claim": "smtp-acceptance-only",
    })
}

/// Frozen audience material a send commits to: member IDs and the
/// digest/ceiling the freeze pinned.
pub struct FrozenAudience {
    pub member_ids: Vec<String>,
    pub digest: String,
    pub max_recipients: i64,
    pub base: AudienceBase,
    pub exclusion_list_id: Option<String>,
}

impl RecordStore {
    /// Record one SMTP-accepted test send — the only evidence a
    /// prepare accepts. Written with the exact content and link
    /// digests the send observed, so an edit or rebind makes the
    /// evidence stale by construction.
    pub fn app_campaign_test_send_record(
        &self,
        context: &str,
        campaign: &str,
        content_digest: &str,
        link_digest: &str,
    ) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO app_campaign_test_sends(context_id,campaign_id,content_digest,link_digest,accepted,at) VALUES(?,?,?,?,1,?) ON CONFLICT(context_id,campaign_id,content_digest,link_digest) DO UPDATE SET accepted=1,at=excluded.at",
            params![context, campaign, content_digest, link_digest, super::now()],
        )
        .map_err(|e| Error::internal(e.to_string()))?;
        Ok(())
    }

    /// Has this exact `(content_digest, link_digest)` pair been
    /// test-sent and SMTP-accepted? Anything less specific is no
    /// evidence — a test send of an older revision or a prior
    /// binding does not count.
    pub fn app_campaign_test_send_accepted(
        &self,
        context: &str,
        campaign: &str,
        content_digest: &str,
        link_digest: &str,
    ) -> Result<bool> {
        let conn = self.conn();
        let accepted: Option<i64> = conn
            .query_row(
                "SELECT accepted FROM app_campaign_test_sends WHERE context_id=? AND campaign_id=? AND content_digest=? AND link_digest=?",
                params![context, campaign, content_digest, link_digest],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?;
        Ok(accepted == Some(1))
    }

    /// The campaign's content is approved at its current revision
    /// exactly when `approval_revision == revision` and
    /// `approval_digest == digest`. Anything else refuses — an edit
    /// after approval is unapproved content.
    pub fn app_content_approved(&self, context: &str, campaign: &str) -> Result<(i64, String)> {
        let conn = self.conn();
        let (revision, digest, approval_revision, approval_digest): (
            i64,
            String,
            Option<i64>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT revision,content_digest,approval_revision,approval_digest FROM app_content_docs WHERE context_id=? AND campaign_id=?",
                params![context, campaign],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
            .ok_or_else(|| {
                Error::rejected("email content is unavailable for this installation and context")
            })?;
        if approval_revision != Some(revision)
            || approval_digest.as_deref() != Some(digest.as_str())
        {
            return Err(Error::rejected(
                "email content is not approved at its current revision",
            ));
        }
        Ok((revision, digest))
    }

    /// The frozen audience a send binds: member IDs, digest, ceiling
    /// and the base/exclusion selectors, so the caller can also
    /// re-verify live validity through `app_audience_show`.
    pub fn app_audience_frozen(&self, context: &str, freeze_id: &str) -> Result<FrozenAudience> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(freeze_id, "freeze ID")?;
        let conn = self.conn();
        let (base_text, exclusion_list_id, members, digest, max_recipients): (
            String,
            Option<String>,
            String,
            String,
            i64,
        ) = conn
            .query_row(
                "SELECT base,exclusion_list_id,member_ids,digest,max_recipients FROM app_audience_freezes WHERE context_id=? AND freeze_id=?",
                params![context, freeze_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
            .ok_or_else(|| {
                Error::rejected("audience freeze is unavailable for this installation and context")
            })?;
        let base_raw: Value = serde_json::from_str(&base_text)
            .map_err(|_| Error::rejected("audience freeze is corrupt"))?;
        let base = AudienceBase::parse(&base_raw)
            .map_err(|_| Error::rejected("audience freeze is corrupt"))?;
        let member_ids: Vec<String> = serde_json::from_str(&members)
            .map_err(|_| Error::rejected("audience freeze is corrupt"))?;
        Ok(FrozenAudience {
            member_ids,
            digest,
            max_recipients,
            base,
            exclusion_list_id,
        })
    }

    /// The `{{first_name|fallback}}` material for one frozen member:
    /// the profile's first display-name word, or `None` for the
    /// block's own fallback. Customer records are the PII custody —
    /// nothing here escapes the file.
    pub fn app_customer_sendable(&self, context: &str, customer_id: &str) -> Result<Sendable> {
        let conn = self.conn();
        let body: Option<String> = conn
            .query_row(
                "SELECT body FROM app_records WHERE context_id=? AND id=? AND kind='customer'",
                params![context, customer_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?;
        let Some(body) = body else {
            return Ok(Sendable::Refused("unknown"));
        };
        let profile = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|stored| CustomerProfile::parse(&stored).ok());
        let Some(profile) = profile else {
            return Ok(Sendable::Refused("invalid"));
        };
        let address = profile.email.clone().unwrap_or_default();
        if profile
            .email
            .as_deref()
            .is_none_or(|email| !email_shape_valid(email))
        {
            return Ok(Sendable::Refused("invalid"));
        }
        match profile.consent.email {
            ConsentState::Denied => return Ok(Sendable::Refused("unsubscribed")),
            ConsentState::Unknown => return Ok(Sendable::Refused("no-consent")),
            ConsentState::Granted => {}
        }
        let suppressed: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM app_suppressions WHERE context_id=? AND ((kind='email' AND key=?) OR (kind='customer' AND key=?)) LIMIT 1",
                params![context, address.to_lowercase(), customer_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?;
        if suppressed.is_some() {
            return Ok(Sendable::Refused("suppressed"));
        }
        let first_name = profile
            .display_name
            .split_whitespace()
            .next()
            .map(str::to_string);
        Ok(Sendable::Ok {
            email: address,
            first_name,
        })
    }

    /// Write the `prepared` send row, idempotent on
    /// `(context, request_id)`: replaying the identical request
    /// returns the stored row; the same request ID behind different
    /// material refuses, and a live `prepared`/`sending` row under a
    /// different request refuses a second prepare of different
    /// material for the same send ID.
    pub fn app_campaign_send_prepare(
        &self,
        context: &str,
        draft: &SendDraft,
    ) -> Result<CampaignSend> {
        // `write_tx` holds `BEGIN IMMEDIATE` across the replay check +
        // insert: a racing prepare blocks on the write lock first, so a
        // request_id binds exactly one frozen material set.
        match self.write_tx(|tx| {
            let existing: Option<CampaignSend> = tx
                .query_opt(
                    &format!("SELECT {SEND_COLUMNS} FROM app_campaign_sends WHERE context_id=? AND request_id=?"),
                    params![context, draft.request_id],
                    row_send,
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if let Some(stored) = existing {
                // A replay binds every frozen field, not just the ID.
                if stored.send_id == draft.send_id
                    && stored.campaign_id == draft.campaign_id
                    && stored.content_revision == draft.content_revision
                    && stored.content_digest == draft.content_digest
                    && stored.audience_freeze_id == draft.audience_freeze_id
                    && stored.audience_digest == draft.audience_digest
                    && stored.connection_id == draft.connection_id
                    && stored.auth_revision == draft.auth_revision
                    && stored.link_revision == draft.link_revision
                    && stored.link_digest == draft.link_digest
                    && stored.max_recipients == draft.max_recipients
                    && stored.send_digest == draft.send_digest
                    && stored.unsubscribe_origin == draft.unsubscribe_origin
                {
                    return Ok(Some(stored));
                }
                return Err(Error::rejected(
                    "campaign send request ID is already used for different material",
                ));
            }
            let now = super::now();
            let changed = tx
                .execute(
                    "INSERT INTO app_campaign_sends(context_id,send_id,campaign_id,request_id,content_revision,content_digest,audience_freeze_id,audience_digest,connection_id,auth_revision,link_revision,link_digest,max_recipients,send_digest,unsubscribe_origin,state,created,updated) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?, 'prepared', ?, ?)",
                    params![
                        context,
                        draft.send_id,
                        draft.campaign_id,
                        draft.request_id,
                        draft.content_revision,
                        draft.content_digest,
                        draft.audience_freeze_id,
                        draft.audience_digest,
                        draft.connection_id,
                        draft.auth_revision,
                        draft.link_revision,
                        draft.link_digest,
                        draft.max_recipients,
                        draft.send_digest,
                        draft.unsubscribe_origin,
                        now,
                        now
                    ],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if changed != 1 {
                return Err(Error::rejected("campaign send ID is already used"));
            }
            Ok(None)
        })? {
            Some(stored) => Ok(stored),
            None => self
                .app_campaign_send(context, &draft.send_id)?
                .ok_or_else(|| Error::internal("campaign send vanished after prepare")),
        }
    }

    /// Read one send row.
    pub fn app_campaign_send(&self, context: &str, send_id: &str) -> Result<Option<CampaignSend>> {
        let conn = self.conn();
        conn.query_row(
            &format!(
                "SELECT {SEND_COLUMNS} FROM app_campaign_sends WHERE context_id=? AND send_id=?"
            ),
            params![context, send_id],
            row_send,
        )
        .optional()
        .map_err(|e| Error::internal(e.to_string()))
    }

    /// Sends for a context, optionally one campaign's, oldest first.
    pub fn app_campaign_sends(
        &self,
        context: &str,
        campaign: Option<&str>,
    ) -> Result<Vec<CampaignSend>> {
        let conn = self.conn();
        let mut out = Vec::new();
        match campaign {
            Some(c) => {
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT {SEND_COLUMNS} FROM app_campaign_sends WHERE context_id=? AND campaign_id=? ORDER BY created"
                    ))
                    .map_err(|e| Error::internal(e.to_string()))?;
                let rows = stmt
                    .query_map(params![context, c], row_send)
                    .map_err(|e| Error::internal(e.to_string()))?;
                for row in rows {
                    out.push(row.map_err(|e| Error::internal(e.to_string()))?);
                }
            }
            None => {
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT {SEND_COLUMNS} FROM app_campaign_sends WHERE context_id=? ORDER BY created"
                    ))
                    .map_err(|e| Error::internal(e.to_string()))?;
                let rows = stmt
                    .query_map(params![context], row_send)
                    .map_err(|e| Error::internal(e.to_string()))?;
                for row in rows {
                    out.push(row.map_err(|e| Error::internal(e.to_string()))?);
                }
            }
        }
        Ok(out)
    }

    /// The approve transaction: `prepared → sending` under CAS (so
    /// two concurrent approves can never both win), then one `queued`
    /// delivery per frozen member — one transaction, so a
    /// half-minted send cannot exist. Unsubscribe tokens are minted
    /// at claim time (`app_unsubscribe_token_record`), never stored.
    pub fn app_campaign_send_approve(
        &self,
        context: &str,
        send_id: &str,
        deliveries: &[(String, String, String)], // (customer_id, email, idempotency_key)
    ) -> Result<()> {
        self.write_tx(|tx| {
            let now = super::now();
            let changed = tx
                .execute(
                    "UPDATE app_campaign_sends SET state='sending', approved_at=?, updated=? WHERE context_id=? AND send_id=? AND state='prepared'",
                    params![now, now, context, send_id],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if changed != 1 {
                return Err(Error::rejected(
                    "campaign send is not awaiting approval — already decided or unknown",
                ));
            }
            for (customer_id, email, idempotency_key) in deliveries {
                tx.execute(
                    "INSERT INTO app_campaign_deliveries(context_id,send_id,customer_id,email,idempotency_key,state,attempts,updated) VALUES(?,?,?,?,?, 'queued', 0, ?)",
                    params![context, send_id, customer_id, email, idempotency_key, now],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            }
            Ok(())
        })
    }

    /// Terminal state on the send row: `completed` or `closed` with
    /// its reason. Never reopens.
    pub fn app_campaign_send_close(
        &self,
        context: &str,
        send_id: &str,
        state: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        debug_assert!(matches!(state, "completed" | "closed"));
        let conn = self.conn();
        conn.execute(
            "UPDATE app_campaign_sends SET state=?, close_reason=?, updated=? WHERE context_id=? AND send_id=?",
            params![state, reason, super::now(), context, send_id],
        )
        .map_err(|e| Error::internal(e.to_string()))?;
        Ok(())
    }

    /// Deliveries of one send in `customer_id` order — the worker's
    /// iteration order, so retries and resumes are deterministic.
    pub fn app_campaign_deliveries(&self, context: &str, send_id: &str) -> Result<Vec<Delivery>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {DELIVERY_COLUMNS} FROM app_campaign_deliveries WHERE context_id=? AND send_id=? ORDER BY customer_id"
            ))
            .map_err(|e| Error::internal(e.to_string()))?;
        let rows = stmt
            .query_map(params![context, send_id], row_delivery)
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::internal(e.to_string()))?);
        }
        Ok(out)
    }

    /// One delivery row.
    pub fn app_campaign_delivery(
        &self,
        context: &str,
        send_id: &str,
        customer_id: &str,
    ) -> Result<Option<Delivery>> {
        let conn = self.conn();
        conn.query_row(
            &format!("SELECT {DELIVERY_COLUMNS} FROM app_campaign_deliveries WHERE context_id=? AND send_id=? AND customer_id=?"),
            params![context, send_id, customer_id],
            row_delivery,
        )
        .optional()
        .map_err(|e| Error::internal(e.to_string()))
    }

    /// Claim one queued row for submission: `queued → submitting`
    /// with the attempt counted, committed BEFORE any socket opens —
    /// the durable marker a crash reconciles to `uncertain`, never
    /// to a silent resend. Returns false when the row is not queued.
    pub fn app_campaign_delivery_claim(
        &self,
        context: &str,
        send_id: &str,
        customer_id: &str,
    ) -> Result<bool> {
        self.write_tx(|tx| {
            let changed = tx
                .execute(
                    "UPDATE app_campaign_deliveries SET state='submitting', attempts=attempts+1, updated=? WHERE context_id=? AND send_id=? AND customer_id=? AND state='queued'",
                    params![super::now(), context, send_id, customer_id],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            Ok(changed == 1)
        })
    }

    /// A `submitting` row that did not reach the wire goes back to
    /// `queued` for a bounded retry; an `uncertain` row never does —
    /// it may already be delivered.
    pub fn app_campaign_delivery_requeue(
        &self,
        context: &str,
        send_id: &str,
        customer_id: &str,
    ) -> Result<()> {
        self.write_tx(|tx| {
            tx.execute(
                "UPDATE app_campaign_deliveries SET state='queued', updated=? WHERE context_id=? AND send_id=? AND customer_id=? AND state='submitting'",
                params![super::now(), context, send_id, customer_id],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            Ok(())
        })
    }

    /// Terminal/suppression state on one delivery, compare-and-set:
    /// the row must sit in one of `from` — otherwise this is a stale
    /// writer racing a finish that already landed, and it refuses.
    #[allow(clippy::too_many_arguments)]
    pub fn app_campaign_delivery_finish(
        &self,
        context: &str,
        send_id: &str,
        customer_id: &str,
        state: &str,
        smtp_code: Option<i64>,
        smtp_message: Option<&str>,
        reason: Option<&str>,
        resolved_by: Option<&str>,
        from: &[&str],
    ) -> Result<()> {
        debug_assert!(matches!(
            state,
            "accepted" | "failed" | "uncertain" | "suppressed" | "closed" | "queued" | "submitting"
        ));
        if from.is_empty()
            || !from
                .iter()
                .all(|s| matches!(*s, "queued" | "submitting" | "uncertain"))
        {
            return Err(Error::internal(
                "delivery transition has no valid source states",
            ));
        }
        // `from` members are whitelisted literals — safe to inline.
        let clause = if from.len() == 1 {
            format!("AND state='{}'", from[0])
        } else {
            format!(
                "AND state IN ({})",
                from.iter()
                    .map(|s| format!("'{s}'"))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        let conn = self.conn();
        let changed = conn
            .execute(
                &format!(
                    "UPDATE app_campaign_deliveries SET state=?, smtp_code=COALESCE(?, smtp_code), smtp_message=COALESCE(?, smtp_message), reason=COALESCE(?, reason), resolved_by=COALESCE(?, resolved_by), updated=? WHERE context_id=? AND send_id=? AND customer_id=? {clause}"
                ),
                params![state, smtp_code, smtp_message, reason, resolved_by, super::now(), context, send_id, customer_id],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        if changed != 1 {
            return Err(Error::rejected(
                "delivery is no longer in a state that allows this transition",
            ));
        }
        Ok(())
    }

    /// Operator reconciliation of an `uncertain` row — the only
    /// resolver, and only toward `accepted`/`failed`. Never resends.
    pub fn app_campaign_delivery_resolve(
        &self,
        context: &str,
        send_id: &str,
        customer_id: &str,
        resolution: &str,
        resolved_by: &str,
    ) -> Result<()> {
        debug_assert!(matches!(resolution, "accepted" | "failed"));
        self.write_tx(|tx| {
            let changed = tx
                .execute(
                    "UPDATE app_campaign_deliveries SET state=?, resolved_by=?, updated=? WHERE context_id=? AND send_id=? AND customer_id=? AND state='uncertain'",
                    params![resolution, resolved_by, super::now(), context, send_id, customer_id],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if changed != 1 {
                return Err(Error::rejected(
                    "only an uncertain delivery can be resolved",
                ));
            }
            Ok(())
        })
    }

    /// Crash sweep inside one record file: every `submitting` row of
    /// the send goes `uncertain` — the daemon restarted mid-submission
    /// and cannot know whether the wire took the message.
    pub fn app_campaign_mark_submitting_uncertain(
        &self,
        context: &str,
        send_id: &str,
    ) -> Result<u64> {
        let changed = self.write_tx(|tx| {
            tx.execute(
                "UPDATE app_campaign_deliveries SET state='uncertain', reason='daemon restarted mid-submission', updated=? WHERE context_id=? AND send_id=? AND state='submitting'",
                params![super::now(), context, send_id],
            )
            .map_err(|e| Error::internal(e.to_string()))
        })?;
        Ok(changed as u64)
    }

    /// Delivery counts by state for one send.
    pub fn app_campaign_delivery_counts(&self, context: &str, send_id: &str) -> Result<Value> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT state, count(*) FROM app_campaign_deliveries WHERE context_id=? AND send_id=? GROUP BY state")
            .map_err(|e| Error::internal(e.to_string()))?;
        let rows = stmt
            .query_map(params![context, send_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut counts = json!({
            "queued": 0, "submitting": 0, "accepted": 0, "failed": 0,
            "uncertain": 0, "suppressed": 0, "closed": 0,
        });
        for row in rows {
            let (state, count) = row.map_err(|e| Error::internal(e.to_string()))?;
            if counts.get(&state).is_some() {
                counts[&state] = json!(count);
            }
        }
        Ok(counts)
    }

    /// Where one unsubscribe token's suppression rows land:
    /// `(context, customer, send)` behind its sha256.
    pub fn app_unsubscribe_token_lookup(
        &self,
        token_hash: &str,
    ) -> Result<Option<(String, String, String)>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT context_id,customer_id,send_id FROM app_unsubscribe_tokens WHERE token_hash=?",
            params![token_hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| Error::internal(e.to_string()))
    }

    /// Record one freshly minted token's hash at claim time, before
    /// the socket opens — a message never leaves whose token would
    /// be unredeemable. Retries mint fresh tokens; earlier hashes
    /// stay valid.
    pub fn app_unsubscribe_token_record(
        &self,
        context: &str,
        send_id: &str,
        customer_id: &str,
        token_hash: &str,
    ) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT OR IGNORE INTO app_unsubscribe_tokens(token_hash,context_id,customer_id,send_id,created) VALUES(?,?,?,?,?)",
            params![token_hash, context, customer_id, send_id, super::now()],
        )
        .map_err(|e| Error::internal(e.to_string()))?;
        Ok(())
    }

    /// Every token hash minted for one send.
    pub fn app_unsubscribe_hashes(&self, context: &str, send_id: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT token_hash FROM app_unsubscribe_tokens WHERE context_id=? AND send_id=?",
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        let rows = stmt
            .query_map(params![context, send_id], |r| r.get::<_, String>(0))
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::internal(e.to_string()))?);
        }
        Ok(out)
    }
}

/// The JSON a show/list emits: the send row plus masked deliveries
/// and state counts — never a full address.
pub fn send_view(
    install: &str,
    context: &str,
    send: &CampaignSend,
    deliveries: &[Delivery],
    counts: Value,
) -> Value {
    json!({
        "send": send_json(install, context, send),
        "counts": counts,
        "deliveries": deliveries.iter().map(delivery_json).collect::<Vec<_>>(),
        "delivery_claim": "smtp-acceptance-only",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masked_email_never_echoes_the_address() {
        assert_eq!(masked_email("alice@example.com"), "a***@example.com");
        assert_eq!(masked_email("b@c.d"), "b***@c.d");
        assert_eq!(masked_email("no-at"), "***");
        for masked in [masked_email("alice@example.com"), masked_email("z@q.io")] {
            assert!(masked.contains('*'), "{masked}");
        }
    }

    #[test]
    fn delivery_key_binds_every_participant() {
        let a = delivery_idempotency_key("i1", "c1", "s1", "cust-1");
        assert_ne!(a, delivery_idempotency_key("i1", "c1", "s1", "cust-2"));
        assert_ne!(a, delivery_idempotency_key("i2", "c1", "s1", "cust-1"));
        assert_ne!(a, delivery_idempotency_key("i1", "c2", "s1", "cust-1"));
        assert_eq!(a, delivery_idempotency_key("i1", "c1", "s1", "cust-1"));
    }

    #[test]
    fn unsubscribe_tokens_are_opaque_bearer_credentials() {
        let token = mint_unsubscribe_token();
        assert_eq!(token.len(), 43, "32 bytes base64url is 43 chars");
        assert!(token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(token, mint_unsubscribe_token());
        let hash = unsubscribe_token_hash(&token);
        assert!(hash.starts_with("sha256:"));
        assert!(!hash.contains(&token[..20]));
    }
}
