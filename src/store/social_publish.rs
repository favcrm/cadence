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

use super::StoreConn;
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
/// CAD-1123 HP4: a queued intent's due time moved under a new approval.
pub const SOCIAL_PUBLISH_RESCHEDULED_EVENT: &str = "social_publish_rescheduled";
pub const SOCIAL_PUBLISH_CLAIMED_EVENT: &str = "social_publish_claimed";
pub const SOCIAL_PUBLISH_REPORTED_EVENT: &str = "social_publish_reported";
/// Minimum server-owned delay between queue commit and either claim path.
pub(crate) const SOCIAL_PUBLISH_UNDO_SECS: i64 = 5;

/// Test-only attach transaction boundaries. The hook receives no transaction
/// or store access and can only synchronize an acceptance fixture.
#[cfg(feature = "test-seam")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocialPublishAttachBoundary {
    BeforeAuthorizationCommit,
    AfterAuthorizationCommitBeforeArm,
}

#[cfg(feature = "test-seam")]
pub type SocialPublishAttachTestHook =
    std::sync::Arc<dyn Fn(SocialPublishAttachBoundary) + Send + Sync>;

#[cfg(feature = "test-seam")]
fn social_publish_attach_test_hooks(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, SocialPublishAttachTestHook>> {
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, SocialPublishAttachTestHook>>,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(feature = "test-seam")]
fn social_publish_attach_test_hook(database_id: &str) -> Option<SocialPublishAttachTestHook> {
    social_publish_attach_test_hooks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(database_id)
        .cloned()
}

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
    /// Server-owned maturity time; distinct from the AOS signed due window.
    pub claim_after_epoch: i64,
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
    if row.claim_after_epoch < 0 {
        return Err(bad("claim maturity time"));
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
        "frozen":frozen,"frozen_digest":digest,"receipt":receipt,"upstream":upstream,
        "permalink":posted_permalink(state,receipt)}})
}

/// CAD-1123 HP4: the public link to the post, only once the intent is
/// `posted` and only from the provider's own evidence on the receipt.
/// It must be a plain https URL on a known social host, so the screen can
/// show "View on Instagram" without trusting free text.
pub fn posted_permalink(state: &str, receipt: Option<&Value>) -> Value {
    if state != "posted" {
        return Value::Null;
    }
    let Some(link) = receipt.and_then(|r| r["permalink"].as_str()) else {
        return Value::Null;
    };
    let host = link
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("");
    let known = matches!(
        host,
        "www.instagram.com" | "instagram.com" | "www.facebook.com" | "facebook.com"
    );
    let plain = link.len() <= 300
        && link
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&b));
    if known && plain {
        json!(link)
    } else {
        Value::Null
    }
}

fn parse_json_cell(cell: Option<String>, what: &str) -> Result<Option<Value>> {
    cell.as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| Error::rejected(format!("social publish {what} is corrupt")))
}

fn read_row(conn: &impl super::StoreConn, intent_id: &str) -> Result<Value> {
    type PublishIntentRow = (
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
        i64,
        i64,
    );
    let row: PublishIntentRow = conn
        .query_row(
            "SELECT intent_id,request,state,frozen,frozen_digest,receipt,upstream,due_epoch,claim_after_epoch,claim_armed FROM social_publish_intents WHERE intent_id=?",
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
                    r.get(8)?,
                    r.get(9)?,
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
    // These columns are the queue/claim SQL source of truth. `due_epoch`
    // remains distinct from the server-owned post-queue undo maturity floor.
    env["intent"]["due_epoch"] = json!(row.7);
    env["intent"]["claim_after_epoch"] = json!(row.8);
    env["intent"]["claim_armed"] = json!(row.9 == 1);
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
        self.write_tx(|conn| {
            let tx = &mut *conn;
            if let Some(existing) = tx
                .query_row_raw(
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
                .query_row_raw(
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
            tx.execute("INSERT INTO social_publish_intents(intent_id,request,install_id,context_id,run_id,effect_id,connection_id,destination_id,toolkit,caption_digest,image_digest,media_key,grant_id,approval_id,due_epoch,claim_after_epoch,timezone,state,frozen,frozen_digest,created,updated) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'queued',?,?,?,?)",
                params![intent_id,request,row.install_id,row.context_id,row.run_id,row.effect_id,row.connection_id,row.destination_id,row.toolkit,row.caption_digest,row.image_digest,row.media_key,row.grant_id,row.approval_id,row.due_epoch,row.claim_after_epoch,row.timezone,frozen.to_string(),digest,now(),now()])?;
            Self::event(
                &tx,
                platform::PLATFORM_STREAM,
                SOCIAL_PUBLISH_SCHEDULED_EVENT,
                json!({"intent_id":intent_id,"request":request,"digest":digest}),
            )?;
            let result = read_row(&tx, &intent_id)?;
            Ok(result)
        })
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
        self.write_tx(|conn| {
            let tx = &mut *conn;
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
            Ok(result)
        })
    }

    /// CAD-1123 HP4: move one queued intent's due time under a NEW approval,
    /// atomically. One UPDATE compare-and-swaps on everything the operator
    /// saw: `state='queued'`, the intent's own install and exact context,
    /// and the due time the screen showed (`expected_due_epoch`). The row is
    /// never cancelled and re-frozen, so there is no window with no
    /// schedule: it either keeps its old time or has the new one. A claim
    /// (driver or send-now), a cancel, a second reschedule that read the same
    /// old time, or a wrong scope changes zero rows and refuses. The new
    /// approval id is the minted `apv-` shape and, like every approval,
    /// authorizes exactly one change (a replay refuses).
    pub fn social_publish_reschedule(
        &self,
        intent_id: &str,
        install_id: &str,
        context_id: Option<&str>,
        expected_due_epoch: i64,
        due_epoch: i64,
        approval_id: &str,
    ) -> Result<Value> {
        if !valid_approval_id(approval_id) {
            return Err(Error::rejected(
                "bad_approval: approval id must be apv- followed by 32 lowercase hex",
            ));
        }
        if due_epoch <= 0 {
            return Err(Error::rejected("social publish due time is invalid"));
        }
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let replayed: Option<String> = tx
                .query_row_raw(
                    "SELECT intent_id FROM social_publish_intents WHERE approval_id=?",
                    [approval_id],
                    |r| r.get(0),
                )
                .optional()?;
            if replayed.is_some() {
                return Err(Error::rejected(
                    "approval_replay: this approval already authorized another social publish intent",
                ));
            }
            let current: Option<(String, i64)> = tx
                .query_row_raw(
                    "SELECT frozen,due_epoch FROM social_publish_intents WHERE intent_id=? AND state='queued' AND install_id=? AND context_id IS ?",
                    params![intent_id, install_id, context_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let refuse = || {
                Error::rejected(
                    "only a queued social publish intent in this install and context, still at the time you saw, can be rescheduled",
                )
            };
            let Some((frozen_text, due)) = current else {
                return Err(refuse());
            };
            if due != expected_due_epoch {
                return Err(refuse());
            }
            let mut frozen: Value = serde_json::from_str(&frozen_text)?;
            frozen["due_epoch"] = json!(due_epoch);
            frozen["approval_id"] = json!(approval_id);
            let digest = app_runs::material_digest(&frozen);
            let changed = tx.execute(
                "UPDATE social_publish_intents SET due_epoch=?,approval_id=?,frozen=?,frozen_digest=?,updated=? WHERE intent_id=? AND state='queued' AND install_id=? AND context_id IS ? AND due_epoch=?",
                params![due_epoch, approval_id, frozen.to_string(), digest, now(), intent_id, install_id, context_id, expected_due_epoch],
            )?;
            if changed != 1 {
                return Err(refuse());
            }
            Self::event(
                &tx,
                platform::PLATFORM_STREAM,
                SOCIAL_PUBLISH_RESCHEDULED_EVENT,
                json!({"intent_id":intent_id,"due_epoch":due_epoch,"approval_id":approval_id}),
            )?;
            read_row(&tx, intent_id)
        })
    }

    /// CAD-1123 HP4: the intent a host-minted request id already froze in
    /// this install, if any — so a retried or double-tapped publish start
    /// resumes the same intent instead of minting a second one.
    pub(crate) fn social_publish_find_request(
        &self,
        install_id: &str,
        request_id: &str,
    ) -> Result<Option<Value>> {
        let request = format!(
            "social-publish-{}",
            uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_OID,
                format!("{install_id}:{request_id}").as_bytes()
            )
            .simple()
        );
        let conn = self.conn();
        let id: Option<String> = conn
            .query_row(
                "SELECT intent_id FROM social_publish_intents WHERE request=?",
                [&request],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| read_row(&conn, &id)).transpose()
    }

    /// CAD-1123 HP4: the artifact a run's independent reviewer approved —
    /// the one the publish material re-proves. Ordered like the material's
    /// own review lookup, so both name the same artifact.
    pub(crate) fn app_run_approved_artifact(&self, run_id: &str) -> Result<String> {
        self.conn()
            .query_row(
                "SELECT artifact_id FROM app_run_reviews WHERE run_id=? AND decision='approve' ORDER BY step_id LIMIT 1",
                [run_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::rejected("run has no independently approved artifact"))
    }

    /// CAD-1143 Redo render seeding: exactly one approved caption —
    /// multiple DISTINCT approved artifacts are ambiguous (no first-pick,
    /// no naive latest); the transaction re-derives the same rule before
    /// freezing. Single-caption sources pass unchanged.
    pub(crate) fn app_run_single_approved_artifact(&self, run_id: &str) -> Result<String> {
        let distinct: i64 = self
            .conn()
            .query_row(
                "SELECT COUNT(DISTINCT artifact_id) FROM app_run_reviews WHERE run_id=? AND decision='approve'",
                [run_id],
                |r| r.get(0),
            )?;
        if distinct == 0 {
            return Err(Error::rejected(
                "carry source has no independently approved caption",
            ));
        }
        if distinct > 1 {
            return Err(Error::rejected(
                "carry source approves more than one caption",
            ));
        }
        self.conn()
            .query_row(
                "SELECT artifact_id FROM app_run_reviews WHERE run_id=? AND decision='approve' ORDER BY step_id LIMIT 1",
                [run_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::rejected("carry source has no independently approved caption"))
    }

    /// CAD-1143 Redo render seeding: the approved review's image pin for
    /// one artifact, if any. The creation transaction re-derives and
    /// asserts it before freezing; this read only seeds the render.
    pub(crate) fn app_run_approved_asset_pin(
        &self,
        run_id: &str,
        artifact_id: &str,
    ) -> Result<Option<(String, String)>> {
        let row: Option<(Option<String>, Option<String>)> = self
            .conn()
            .query_row(
                "SELECT asset_receipt_id, asset_digest FROM app_run_reviews WHERE run_id=? AND artifact_id=? AND decision='approve' ORDER BY step_id LIMIT 1",
                params![run_id, artifact_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(receipt, digest)| receipt.zip(digest)))
    }

    pub fn social_publish_show(&self, intent_id: &str) -> Result<Value> {
        read_row(&self.conn(), intent_id)
    }

    /// CAD-1129 H5: cancel every queued publish intent of one install
    /// — the soft-remove path. Unlike `social_publish_cancel` this is
    /// install-wide and idempotent; an install with nothing queued
    /// returns 0. Queued-only is deliberate: a `processing` intent is
    /// already committed at the platform and is never un-sent here.
    pub fn social_publish_cancel_install(&self, install_id: &str) -> Result<i64> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let intents = tx.query_vec(
                "SELECT intent_id FROM social_publish_intents WHERE install_id=? AND state='queued'",
                [install_id],
                |r| r.get::<_, String>(0),
            )?;
            let mut changed = 0i64;
            for id in &intents {
                changed += tx.execute(
                    "UPDATE social_publish_intents SET state='cancelled',updated=? WHERE intent_id=? AND state='queued'",
                    params![now(), id],
                )? as i64;
                Self::event(
                    &tx,
                    platform::PLATFORM_STREAM,
                    SOCIAL_PUBLISH_CANCELLED_EVENT,
                    json!({"intent_id": id}),
                )?;
            }
            Ok(changed)
        })
    }

    pub fn social_publish_list(
        &self,
        install: Option<&str>,
        context: Option<&str>,
    ) -> Result<Value> {
        if context.is_some() && install.is_none() {
            return Err(Error::rejected("context filter requires installation"));
        }
        self.read_tx(|conn| {

                    let stmt_sql = "SELECT intent_id FROM social_publish_intents WHERE (? IS NULL OR install_id=?) AND (? IS NULL OR context_id=?) ORDER BY intent_id LIMIT 100";
                    let ids = conn.query_vec(stmt_sql, params![install, install, context, context], |r| {
                            r.get::<_, String>(0)
                        }).map(|rows| rows.into_iter().map(Ok::<_, rusqlite::Error>))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    let mut list = Vec::new();
                    for id in ids {
                        list.push(read_row(&conn, &id)?["intent"].clone());
                    }
                    Ok(json!({"intents":list}))
        })
    }

    /// Peek the oldest due AND mature queued intent without claiming. The
    /// dispatch RPC compares operator-supplied current authority against
    /// frozen before claiming; the claim itself re-verifies in-transaction.
    pub(crate) fn social_publish_peek_due(&self, now_epoch: i64) -> Result<Option<Value>> {
        self.read_tx(|conn| {

                    let next: Option<String> = conn
                        .query_opt(
                            "SELECT intent_id FROM social_publish_intents WHERE state='queued' AND claim_armed=1 AND due_epoch<=? AND claim_after_epoch<=? ORDER BY due_epoch,intent_id LIMIT 1",
                            [now_epoch, now_epoch],
                            |r| r.get(0),
                        )?;
                    next.map(|id| read_row(&conn, &id)).transpose()
        })
    }

    /// CAD-1020: one page of the driver's due rows — up to `limit` due and
    /// mature queued intents, oldest first, after the `(due_epoch, intent_id)`
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
            "SELECT intent_id FROM social_publish_intents WHERE state='queued' AND claim_armed=1 AND due_epoch<=?1 AND claim_after_epoch<=?1 AND (due_epoch>?3 OR (due_epoch=?3 AND intent_id>?4)) ORDER BY due_epoch,intent_id LIMIT ?2",
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

    /// Atomically claim the oldest due and mature queued intent for dispatch.
    /// The caller proves current authority through `eligible` (grant, binding,
    /// app/context eligibility and unchanged digests at dispatch): a false
    /// verdict leaves the row queued. Exactly one claimant wins.
    pub(crate) fn social_publish_claim_due<F>(
        &self,
        now_epoch: i64,
        eligible: F,
    ) -> Result<Option<Value>>
    where
        F: FnOnce(&super::WriteTxn<'_>, &str, &Value) -> Result<bool>,
    {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let next: Option<String> = tx
                .query_row_raw(
                    "SELECT intent_id FROM social_publish_intents WHERE state='queued' AND claim_armed=1 AND due_epoch<=? AND claim_after_epoch<=? ORDER BY due_epoch,intent_id LIMIT 1",
                    [now_epoch, now_epoch],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(id) = next else {
                return Ok(None);
            };
            let frozen_text: String = tx.query_row_raw(
                "SELECT frozen FROM social_publish_intents WHERE intent_id=?",
                [&id],
                |r| r.get(0),
            )?;
            let frozen: Value = serde_json::from_str(&frozen_text)?;
            // The candidate intent_id passes too: a caller that peeked one
            // id can pin its claim to that exact row — a queue-head move
            // between peek and claim is a no-claim, never a send of an
            // intent the caller never inspected.
            if !eligible(tx, &id, &frozen)? {
                return Ok(None);
            }
            let changed = tx.execute("UPDATE social_publish_intents SET state='processing',updated=? WHERE intent_id=? AND state='queued' AND claim_armed=1 AND due_epoch<=? AND claim_after_epoch<=?",params![now(),id,now_epoch,now_epoch])?;
            if changed != 1 {
                return Ok(None);
            }
            Self::event(
                &tx,
                platform::PLATFORM_STREAM,
                SOCIAL_PUBLISH_CLAIMED_EVENT,
                json!({"intent_id":id}),
            )?;
            let result = read_row(&tx, &id)?;
            Ok(Some(result))
        })
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
    /// over-stale). The owner-attach maturity floor still applies. The install
    /// and exact context are checked against the
    /// row inside the same compare-and-set as the state, so exactly one
    /// claimant wins and a wrong scope never claims; a second click, a
    /// racing `claim_due` or a cancel reads a non-queued row and yields
    /// `None`.
    pub(crate) fn social_publish_claim_id(
        &self,
        intent_id: &str,
        install_id: &str,
        context_id: Option<&str>,
        now_epoch: i64,
    ) -> Result<Option<Value>> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let changed = tx.execute(
                "UPDATE social_publish_intents SET state='processing',updated=? WHERE intent_id=? AND state='queued' AND claim_armed=1 AND install_id=? AND context_id IS ? AND claim_after_epoch<=?",
                params![now(), intent_id, install_id, context_id, now_epoch],
            )?;
            if changed != 1 {
                return Ok(None);
            }
            Self::event(
                &tx,
                platform::PLATFORM_STREAM,
                SOCIAL_PUBLISH_CLAIMED_EVENT,
                json!({"intent_id":intent_id}),
            )?;
            let result = read_row(&tx, intent_id)?;
            Ok(Some(result))
        })
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
        self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let frozen_text: Option<String> = tx
                        .query_opt(
                            "SELECT frozen FROM social_publish_intents WHERE intent_id=? AND state='processing'",
                            [intent_id],
                            |r| r.get(0),
                        )?;
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
                    Ok(result)
        })
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
        self.write_tx(|conn| {

                    let tx = &mut *conn;
                    if state == "posted" {
                        let row: Option<(String, Option<String>)> = tx
                            .query_opt(
                                "SELECT frozen,upstream FROM social_publish_intents WHERE intent_id=? AND state='processing'",
                                [intent_id],
                                |r| Ok((r.get(0)?, r.get(1)?)),
                            )?;
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
                    Ok(result)
        })
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
    pub claim_after_epoch: i64,
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
            claim_after_epoch: row.claim_after_epoch,
            timezone: row.timezone,
        })
    }
}

// ===========================================================================
// CAD-1143: prepared immutable owner intents — an ACCOUNT-ONLY publication
// intent with NO send grant, held in its own table so the queued-only dispatch
// SQL can never see it. `prepare` records a stable, content-bound,
// non-dispatchable intent; `attach` is the atomic compare-and-swap that
// carries an actual owner-exchange grant across PREPARED -> AUTHORIZED.
// Neither path mints, writes, or inherits a grant onto the binding, and
// neither queues, claims or sends. A separate private descriptor exchange
// (AOS contract pending) is what supplies the grant this table stores only
// after it is validated.
// ===========================================================================

/// The prepared-intent table. Deliberately a SEPARATE table from
/// `social_publish_intents`: the due-worker's queued-only SQL
/// (`state='queued'`) selects only `social_publish_intents`, so a PREPARED
/// row here is structurally invisible to every claim/dispatch path. No
/// `grant_id` column is optional-forged — the grant arrives only at
/// `attach`, never at `prepare`.
pub(crate) const SCHEMA_PREPARED: &str = "
CREATE TABLE IF NOT EXISTS social_publish_prepared(
 prepared_id TEXT PRIMARY KEY, request TEXT NOT NULL UNIQUE,
 install_id TEXT NOT NULL, context_id TEXT, run_id TEXT NOT NULL,
 effect_id TEXT NOT NULL,
 connection_id TEXT NOT NULL, aos_connection_id TEXT,
 destination_id TEXT NOT NULL, destination_label TEXT NOT NULL,
 toolkit TEXT NOT NULL, timezone TEXT NOT NULL,
 caption_digest TEXT NOT NULL, image_digest TEXT, media_key TEXT,
 approval_id TEXT NOT NULL,
 mode TEXT NOT NULL CHECK(mode IN ('now','schedule')),
 due_epoch INTEGER NOT NULL CHECK(due_epoch>0),
 not_before_epoch INTEGER NOT NULL, expires_epoch INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('prepared','authorized','cancelled','superseded','refused')),
 grant_id TEXT, descriptor TEXT NOT NULL, descriptor_digest TEXT NOT NULL,
 created REAL NOT NULL, updated REAL NOT NULL);
CREATE INDEX IF NOT EXISTS social_publish_prepared_scope
 ON social_publish_prepared(install_id,context_id,prepared_id);
";

/// Event kinds for the prepared-intent lifecycle (own stream, never the
/// dispatch path's).
pub const SOCIAL_PUBLISH_PREPARED_EVENT: &str = "social_publish_prepared";
pub const SOCIAL_PUBLISH_AUTHORIZED_EVENT: &str = "social_publish_authorized";
pub const SOCIAL_PUBLISH_PREPARED_CANCELLED_EVENT: &str = "social_publish_prepared_cancelled";

/// The four-field ACCOUNT representation — destination id, derived label,
/// toolkit and timezone — with NO grant. A `prepare` carries exactly these
/// fields and nothing else; a `grant_id` (or any other key) never parses,
/// so a forged authority-bearing field cannot slip into the account row.
/// Distinct from `PublishTarget` (the legacy five-field, grant-bearing
/// receipt) which is unchanged.
#[allow(clippy::too_many_arguments)]
pub struct PublishAccount {
    pub destination_id: String,
    pub destination_label: String,
    pub toolkit: String,
    pub timezone: String,
}

impl PublishAccount {
    const FIELDS: [&'static str; 4] =
        ["destination_id", "destination_label", "toolkit", "timezone"];

    /// Validate a candidate account object. Exactly the four fields, all
    /// present and well-formed; a `grant_id`, `connection_id`, owner,
    /// request or any other key never parses — the account carries no
    /// send or workspace authority.
    pub fn parse(account: &Value) -> Result<Self> {
        use crate::platform::agenticos_external::publish as device;
        let object = account
            .as_object()
            .ok_or_else(|| Error::rejected("publish account must be an object"))?;
        if object.len() != Self::FIELDS.len()
            || object.keys().any(|k| !Self::FIELDS.contains(&k.as_str()))
        {
            return Err(Error::rejected(
                "publish account needs exactly destination_id, destination_label, toolkit and timezone (no grant)",
            ));
        }
        let text = |field: &str| {
            account[field]
                .as_str()
                .ok_or_else(|| Error::rejected(format!("publish account {field} must be a string")))
        };
        let (id, label, toolkit, tz) = (
            text("destination_id")?,
            text("destination_label")?,
            text("toolkit")?,
            text("timezone")?,
        );
        let bad = |what: &str| {
            Err(Error::rejected(format!(
                "publish account {what} is invalid"
            )))
        };
        if !(1..=120).contains(&id.len())
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return bad("destination_id");
        }
        if label.trim().is_empty()
            || label.chars().count() > 80
            || label.chars().any(char::is_control)
        {
            return bad("destination_label");
        }
        if device::Toolkit::parse(toolkit).is_none() {
            return bad("toolkit");
        }
        if tz.is_empty()
            || tz.len() > 64
            || !tz
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
        {
            return bad("timezone");
        }
        Ok(Self {
            destination_id: id.into(),
            destination_label: label.into(),
            toolkit: toolkit.into(),
            timezone: tz.into(),
        })
    }
}

/// Params for `social_publish_prepare_intent`. The descriptor is the
/// daemon-assembled, content-bound record of what the owner authorizes —
/// its digest is the identity a changed run/material/window/receipt
/// refuses on. No grant, no caller artifact, no workspace.
#[allow(clippy::too_many_arguments)]
pub struct NewPreparedIntent<'a> {
    pub request_id: &'a str,
    pub install_id: &'a str,
    pub context_id: Option<&'a str>,
    pub run_id: &'a str,
    pub effect_id: &'a str,
    pub connection_id: &'a str,
    pub aos_connection_id: Option<&'a str>,
    pub account: &'a PublishAccount,
    pub caption_digest: &'a str,
    pub image_digest: Option<&'a str>,
    pub media_key: Option<&'a str>,
    pub approval_id: &'a str,
    pub mode: &'a str,
    pub due_epoch: i64,
    pub not_before_epoch: i64,
    pub expires_epoch: i64,
    /// The daemon-assembled descriptor (already validated, content-bound).
    pub descriptor: &'a Value,
}

fn prepared_row_of(row: &NewPreparedIntent<'_>) -> Result<Value> {
    let mut descriptor = row.descriptor.clone();
    if descriptor.get("grant_id").is_some() {
        return Err(Error::rejected(
            "prepared intent descriptor must not contain a grant",
        ));
    }
    let object = descriptor
        .as_object_mut()
        .ok_or_else(|| Error::rejected("prepared intent descriptor must be an object"))?;
    let bound = json!({
        "schema": 1,
        "install_id": row.install_id,
        "context_id": row.context_id,
        "run_id": row.run_id,
        "effect_id": row.effect_id,
        "binding_connection_id": row.connection_id,
        "aos_connection_id": row.aos_connection_id,
        "destination_id": row.account.destination_id,
        "destination_label": row.account.destination_label,
        "toolkit": row.account.toolkit,
        "timezone": row.account.timezone,
        "caption_digest": row.caption_digest,
        "image_digest": row.image_digest,
        "media_key": row.media_key,
        "approval_id": row.approval_id,
        "mode": row.mode,
        "due_epoch": row.due_epoch,
        "not_before_epoch": row.not_before_epoch,
        "expires_epoch": row.expires_epoch,
    });
    for (key, value) in bound.as_object().into_iter().flatten() {
        object.insert(key.clone(), value.clone());
    }
    Ok(descriptor)
}

fn prepared_envelope(
    prepared_id: &str,
    request: &str,
    state: &str,
    descriptor: &Value,
    digest: &str,
) -> Value {
    // Never project the attached grant itself; consumers receive only the
    // intent state and content digest.
    json!({"prepared":{"schema":1,"prepared_id":prepared_id,"request":request,"state":state,
        "descriptor":descriptor,"descriptor_digest":digest}})
}

fn read_prepared_row(conn: &impl super::StoreConn, prepared_id: &str) -> Result<Value> {
    let row: (String, String, String, String, String) = conn
        .query_row(
            "SELECT prepared_id,request,state,descriptor,descriptor_digest FROM social_publish_prepared WHERE prepared_id=?",
            [prepared_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
        .ok_or_else(|| Error::rejected("prepared publish intent does not exist"))?;
    let descriptor: Value = serde_json::from_str(&row.3)?;
    // Re-derive and compare the descriptor receipt; corrupt or changed
    // persisted content fails closed, never trusts the digest column.
    let digest = app_runs::material_digest(&descriptor);
    if digest != row.4 {
        return Err(Error::rejected(
            "prepared intent descriptor receipt is corrupt",
        ));
    }
    Ok(prepared_envelope(
        &row.0,
        &row.1,
        &row.2,
        &descriptor,
        &digest,
    ))
}

fn descriptor_string<'a>(descriptor: &'a Value, field: &str) -> Result<&'a str> {
    descriptor
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::rejected("prepared publish descriptor is corrupt"))
}

fn descriptor_optional_string<'a>(descriptor: &'a Value, field: &str) -> Result<Option<&'a str>> {
    match descriptor.get(field) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(Error::rejected("prepared publish descriptor is corrupt")),
    }
}

fn prepared_send_row<'a>(
    request: &'a str,
    descriptor: &'a Value,
    grant_id: &'a str,
    claim_after_epoch: i64,
) -> Result<NewSocialPublish<'a>> {
    let context_id = descriptor_optional_string(descriptor, "context_id")?;
    let image_digest = descriptor_optional_string(descriptor, "image_digest")?;
    let media_key = descriptor_optional_string(descriptor, "media_key")?;
    let due_epoch = descriptor
        .get("due_epoch")
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::rejected("prepared publish due time is corrupt"))?;
    Ok(NewSocialPublish {
        request_id: request,
        install_id: descriptor_string(descriptor, "install_id")?,
        context_id,
        run_id: descriptor_string(descriptor, "run_id")?,
        effect_id: descriptor_string(descriptor, "effect_id")?,
        artifact_id: Some(descriptor_string(descriptor, "artifact_id")?),
        bundle_digest: Some(descriptor_string(descriptor, "bundle_digest")?),
        slot: Some(descriptor_string(descriptor, "slot")?),
        connection_id: descriptor_string(descriptor, "binding_connection_id")?,
        aos_connection_id: Some(descriptor_string(descriptor, "aos_connection_id")?),
        destination_id: descriptor_string(descriptor, "destination_id")?,
        toolkit: descriptor_string(descriptor, "toolkit")?,
        caption_digest: descriptor_string(descriptor, "caption_digest")?,
        image_digest,
        media_key,
        grant_id,
        approval_id: descriptor_string(descriptor, "approval_id")?,
        due_epoch,
        claim_after_epoch,
        timezone: descriptor_string(descriptor, "timezone")?,
    })
}

fn prepared_status_output(
    conn: &impl super::StoreConn,
    prepared_id: &str,
    install_id: &str,
    context_id: Option<&str>,
) -> Result<Value> {
    let prepared: Option<(String, String)> = conn
        .query_row(
            "SELECT request,state FROM social_publish_prepared WHERE prepared_id=? AND install_id=? AND context_id IS ?",
            params![prepared_id,install_id,context_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (request, state) = prepared.ok_or_else(|| {
        Error::rejected("no prepared publish intent with this id in this install and context")
    })?;
    // Validate the durable descriptor receipt without projecting the
    // descriptor itself into the status response.
    let _ = read_prepared_row(conn, prepared_id)?;
    type PreparedQueueStatusRow = (String, String, i64, i64, i64, String, Option<String>);
    let queued: Option<PreparedQueueStatusRow> = conn
        .query_row(
            "SELECT intent_id,state,due_epoch,claim_after_epoch,claim_armed,install_id,context_id FROM social_publish_intents WHERE request=?",
            [&request],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
        )
        .optional()?;
    if state == "prepared" && queued.is_some() {
        return Err(Error::rejected(
            "prepared intent unexpectedly has queued-send state",
        ));
    }
    if state == "authorized" && queued.is_none() {
        return Err(Error::rejected(
            "authorized prepared intent has no queued send row",
        ));
    }
    let queued = queued
        .map(
            |(
                intent_id,
                queue_state,
                due_epoch,
                claim_after,
                armed,
                queue_install,
                queue_context,
            )| {
                if queue_install != install_id || queue_context.as_deref() != context_id {
                    return Err(Error::rejected("prepared intent queue scope is corrupt"));
                }
                if armed != 0 && armed != 1 || (armed == 1 && claim_after <= 0) {
                    return Err(Error::rejected("prepared intent queue maturity is corrupt"));
                }
                Ok(json!({
                    "intent_id": intent_id,
                    "state": queue_state,
                    "due_epoch": due_epoch,
                    "claim_armed": armed == 1,
                    "claim_after_epoch": if armed == 1 { json!(claim_after) } else { Value::Null },
                }))
            },
        )
        .transpose()?;
    if state == "cancelled"
        && queued
            .as_ref()
            .is_some_and(|queue| queue["state"] != "cancelled")
    {
        return Err(Error::rejected(
            "cancelled prepared intent still has live queued-send state",
        ));
    }
    Ok(json!({
        "prepared_id": prepared_id,
        "install_id": install_id,
        "context_id": context_id,
        "state": state,
        "queued": queued,
    }))
}

fn attached_output(
    conn: &impl super::StoreConn,
    prepared_id: &str,
    install_id: &str,
    context_id: Option<&str>,
) -> Result<Value> {
    let row: (String, String, String, String, String) = conn
        .query_row(
            "SELECT request,state,grant_id,descriptor,descriptor_digest FROM social_publish_prepared WHERE prepared_id=? AND install_id=? AND context_id IS ?",
            params![prepared_id, install_id, context_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_,Option<String>>(2)?.unwrap_or_default(), r.get(3)?, r.get(4)?)),
        )
        .optional()?
        .ok_or_else(|| Error::rejected("no prepared publish intent with this id in this install and context"))?;
    if row.1 != "authorized"
        || !crate::platform::agenticos_external::publish::valid_grant_id(&row.2)
        || !crate::platform::agenticos_external::publish::valid_idempotency_key(&row.0)
    {
        return Err(Error::rejected(
            "prepared publish intent has no authorized queue handoff",
        ));
    }
    let descriptor: Value = serde_json::from_str(&row.3)
        .map_err(|_| Error::rejected("prepared publish descriptor is corrupt"))?;
    let descriptor_digest = app_runs::material_digest(&descriptor);
    if descriptor_digest != row.4 {
        return Err(Error::rejected(
            "prepared intent descriptor receipt is corrupt",
        ));
    }
    let queued_row: Option<(String, String, i64, i64)> = conn
        .query_row(
            "SELECT intent_id,state,claim_after_epoch,claim_armed FROM social_publish_intents WHERE request=?",
            [&row.0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (queued_id, _queue_state, claim_after_epoch, claim_armed) = queued_row
        .ok_or_else(|| Error::rejected("authorized prepared intent has no queued send row"))?;
    if claim_armed != 1 || claim_after_epoch <= 0 {
        return Err(Error::rejected(
            "authorized prepared intent queue is not armed after its undo floor",
        ));
    }
    let send_row = prepared_send_row(&row.0, &descriptor, &row.2, claim_after_epoch)?;
    validate_new(&send_row)?;
    let expected_frozen = frozen_of(&send_row);
    let expected_digest = app_runs::material_digest(&expected_frozen);
    let queued = read_row(conn, &queued_id)?;
    if queued["intent"]["request"] != row.0
        || queued["intent"]["frozen"] != expected_frozen
        || queued["intent"]["frozen_digest"] != expected_digest
        || queued["intent"]["due_epoch"].as_i64() != Some(send_row.due_epoch)
    {
        return Err(Error::rejected(
            "authorized prepared intent queue handoff is corrupt",
        ));
    }
    let prepared = read_prepared_row(conn, prepared_id)?;
    Ok(json!({
        "prepared": prepared["prepared"].clone(),
        "queued": {
            "intent_id": queued_id,
            "request": row.0,
            "state": queued["intent"]["state"].clone(),
            "due_epoch": queued["intent"]["due_epoch"].clone(),
            "claim_after_epoch": queued["intent"]["claim_after_epoch"].clone(),
            "claim_armed": queued["intent"]["claim_armed"].clone(),
        }
    }))
}

fn authorized_queue_recovery_id(
    conn: &impl super::StoreConn,
    prepared_id: &str,
    install_id: &str,
    context_id: Option<&str>,
    request: &str,
    grant_id: &str,
    descriptor: &Value,
) -> Result<String> {
    let descriptor_digest = app_runs::material_digest(descriptor);
    let current: Option<(String, String, Option<String>, String, String)> = conn
        .query_row(
            "SELECT request,state,grant_id,descriptor,descriptor_digest FROM social_publish_prepared WHERE prepared_id=? AND install_id=? AND context_id IS ?",
            params![prepared_id,install_id,context_id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        )
        .optional()?;
    let Some((stored_request, state, stored_grant, stored_descriptor, stored_digest)) = current
    else {
        return Err(Error::rejected(
            "authorized prepared intent is outside this scope",
        ));
    };
    let stored_descriptor_value: Value = serde_json::from_str(&stored_descriptor)?;
    if state != "authorized"
        || stored_request != request
        || stored_grant.as_deref() != Some(grant_id)
        || stored_digest != descriptor_digest
        || &stored_descriptor_value != descriptor
    {
        return Err(Error::rejected(
            "authorized prepared intent changed before queue recovery",
        ));
    }

    type AuthorizedQueueRecoveryRow = (
        String,
        String,
        String,
        i64,
        i64,
        i64,
        String,
        Option<String>,
        String,
        String,
    );
    let queued: Option<AuthorizedQueueRecoveryRow> = conn
        .query_row(
            "SELECT intent_id,state,grant_id,due_epoch,claim_after_epoch,claim_armed,install_id,context_id,frozen,frozen_digest FROM social_publish_intents WHERE request=?",
            [request],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)),
        )
        .optional()?;
    let Some((
        intent_id,
        queue_state,
        queue_grant,
        due_epoch,
        claim_after,
        armed,
        queue_install,
        queue_context,
        frozen_text,
        frozen_digest,
    )) = queued
    else {
        return Err(Error::rejected(
            "authorized prepared intent has no queued recovery row",
        ));
    };
    let valid_intent_id = intent_id.strip_prefix("spub-").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if !valid_intent_id
        || queue_state != "queued"
        || queue_grant != grant_id
        || queue_install != install_id
        || queue_context.as_deref() != context_id
        || (armed == 0 && claim_after != 0)
        || (armed == 1 && claim_after <= 0)
        || (armed != 0 && armed != 1)
    {
        return Err(Error::rejected(
            "authorized prepared intent queue row is not safely recoverable",
        ));
    }
    let floor = if armed == 1 { claim_after } else { 0 };
    let send_row = prepared_send_row(request, descriptor, grant_id, floor)?;
    validate_new(&send_row)?;
    let expected_frozen = frozen_of(&send_row);
    let expected_digest = app_runs::material_digest(&expected_frozen);
    let frozen: Value = serde_json::from_str(&frozen_text)
        .map_err(|_| Error::rejected("authorized prepared queue row is corrupt"))?;
    if due_epoch != send_row.due_epoch
        || frozen != expected_frozen
        || frozen_digest != expected_digest
    {
        return Err(Error::rejected(
            "authorized prepared queue row differs from its immutable descriptor",
        ));
    }
    let shown = read_row(conn, &intent_id)?;
    if shown["intent"]["request"] != request
        || shown["intent"]["state"] != "queued"
        || shown["intent"]["frozen"]["grant_id"] != grant_id
        || shown["intent"]["frozen"]["install_id"] != install_id
        || shown["intent"]["frozen"]["context_id"]
            != context_id.map(Value::from).unwrap_or(Value::Null)
        || shown["intent"]["frozen"] != expected_frozen
        || shown["intent"]["frozen_digest"] != expected_digest
        || shown["intent"]["due_epoch"].as_i64() != Some(due_epoch)
        || shown["intent"]["claim_armed"] != Value::Bool(armed == 1)
        || shown["intent"]["claim_after_epoch"].as_i64() != Some(claim_after)
    {
        return Err(Error::rejected(
            "authorized prepared queue recovery row changed before arm",
        ));
    }
    Ok(intent_id)
}

impl Store {
    /// Durably store a PREPARED immutable intent — account-only, no grant,
    /// never dispatched. Idempotent on the host-minted `request`: the same
    /// request with the same descriptor returns the same row; the same
    /// request with a different descriptor (changed run/material/window/
    /// receipt) refuses rather than forking the key.
    pub fn social_publish_prepare_intent(&self, row: &NewPreparedIntent<'_>) -> Result<Value> {
        // Bound the request id and re-derive the stable request key and the
        // descriptor digest — both content-bound, never caller-trusted.
        crate::proto::identifier(row.request_id, "publish prepare request id")?;
        if row.run_id.is_empty() || row.effect_id.is_empty() || row.install_id.is_empty() {
            return Err(Error::rejected("prepared intent run identity is invalid"));
        }
        if !valid_approval_id(row.approval_id) {
            return Err(Error::rejected(
                "bad_approval: approval id must be apv- followed by 32 lowercase hex",
            ));
        }
        if !matches!(row.mode, "now" | "schedule") {
            return Err(Error::rejected(
                "prepared intent mode must be now or schedule",
            ));
        }
        if !(row.not_before_epoch > 0
            && row.due_epoch > 0
            && row.due_epoch >= row.not_before_epoch
            && row.due_epoch <= row.expires_epoch)
        {
            return Err(Error::rejected(
                "prepared intent window is invalid (due must sit within not_before..=expires)",
            ));
        }
        let request = format!(
            "social-publish-prepared-{}",
            uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_OID,
                format!("{}:{}", row.install_id, row.request_id).as_bytes()
            )
            .simple()
        );
        let descriptor = prepared_row_of(row)?;
        let descriptor_digest = app_runs::material_digest(&descriptor);
        self.write_tx(|conn| {
            let tx = &mut *conn;
            if let Some(existing) = tx
                .query_row_raw(
                    "SELECT prepared_id,request,state,descriptor,descriptor_digest FROM social_publish_prepared WHERE request=?",
                    [&request],
                    |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)),
                )
                .optional()?
            {
                let existing_descriptor: Value = serde_json::from_str(&existing.3)?;
                let existing_digest = app_runs::material_digest(&existing_descriptor);
                if existing_digest != existing.4 || existing_digest != descriptor_digest {
                    return Err(Error::rejected(
                        "publish prepare request already names different or corrupt immutable intent",
                    ));
                }
                return Ok(prepared_envelope(
                    &existing.0, &existing.1, &existing.2, &existing_descriptor,
                    &existing_digest,
                ));
            }
            // A UI reload can mint a fresh request id while retrying the
            // same run/mode/window. Reuse the one active scoped identity
            // rather than creating another owner action. Schedule matches
            // its exact due time; send-now has one active intent per run.
            let scope_count: i64 = tx.query_row_raw(
                "SELECT count(*) FROM social_publish_prepared WHERE install_id=? AND context_id IS ? AND run_id=? AND mode=? AND state IN ('prepared','authorized') AND (?='now' OR due_epoch=?)",
                params![row.install_id,row.context_id,row.run_id,row.mode,row.mode,row.due_epoch],
                |r| r.get(0),
            )?;
            if scope_count > 1 {
                return Err(Error::rejected(
                    "multiple active prepared intents match this run scope",
                ));
            }
            if scope_count == 1 {
                let existing: (String, String, String, String, String) = tx.query_row_raw(
                    "SELECT prepared_id,request,state,descriptor,descriptor_digest FROM social_publish_prepared WHERE install_id=? AND context_id IS ? AND run_id=? AND mode=? AND state IN ('prepared','authorized') AND (?='now' OR due_epoch=?)",
                    params![row.install_id,row.context_id,row.run_id,row.mode,row.mode,row.due_epoch],
                    |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
                )?;
                let existing_descriptor: Value = serde_json::from_str(&existing.3)?;
                let existing_digest = app_runs::material_digest(&existing_descriptor);
                if existing_digest != existing.4 || existing_digest != descriptor_digest {
                    return Err(Error::rejected(
                        "prepared run scope already names different or corrupt immutable intent",
                    ));
                }
                return Ok(prepared_envelope(
                    &existing.0, &existing.1, &existing.2, &existing_descriptor,
                    &existing_digest,
                ));
            }
            let prepared_id = format!("sprep-{}", uuid::Uuid::new_v4().simple());
            tx.execute("INSERT INTO social_publish_prepared(prepared_id,request,install_id,context_id,run_id,effect_id,connection_id,aos_connection_id,destination_id,destination_label,toolkit,timezone,caption_digest,image_digest,media_key,approval_id,mode,due_epoch,not_before_epoch,expires_epoch,state,descriptor,descriptor_digest,created,updated) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'prepared',?,?,?,?)",
                params![prepared_id,request,row.install_id,row.context_id,row.run_id,row.effect_id,row.connection_id,row.aos_connection_id,row.account.destination_id,row.account.destination_label,row.account.toolkit,row.account.timezone,row.caption_digest,row.image_digest,row.media_key,row.approval_id,row.mode,row.due_epoch,row.not_before_epoch,row.expires_epoch,descriptor.to_string(),descriptor_digest,now(),now()])?;
            Self::event(
                &tx,
                platform::PLATFORM_STREAM,
                SOCIAL_PUBLISH_PREPARED_EVENT,
                json!({"prepared_id":prepared_id,"request":request,"digest":descriptor_digest}),
            )?;
            read_prepared_row(&tx, &prepared_id)
        })
    }

    /// The PREPARED intent a host-minted request already froze in this
    /// install, if any — so a retried/double-tapped prepare resumes the
    /// same intent (stable identity), and `attach` can re-load it.
    pub(crate) fn social_publish_prepared_find_request(
        &self,
        install_id: &str,
        request_id: &str,
    ) -> Result<Option<Value>> {
        let request = format!(
            "social-publish-prepared-{}",
            uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_OID,
                format!("{install_id}:{request_id}").as_bytes()
            )
            .simple()
        );
        let conn = self.conn();
        let id: Option<String> = conn
            .query_row(
                "SELECT prepared_id FROM social_publish_prepared WHERE request=?",
                [&request],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| read_prepared_row(&conn, &id)).transpose()
    }

    /// Find a single active prepared row for the exact run scope. This is
    /// the recovery key when a native surface reloads and loses its
    /// in-memory request id; ambiguity refuses rather than choosing one.
    pub(crate) fn social_publish_prepared_find_scope(
        &self,
        install_id: &str,
        context_id: Option<&str>,
        run_id: &str,
        mode: &str,
        due_epoch: Option<i64>,
    ) -> Result<Option<Value>> {
        if !matches!(mode, "now" | "schedule") || (mode == "schedule") != due_epoch.is_some() {
            return Err(Error::rejected("prepared run recovery scope is invalid"));
        }
        let conn = self.conn();
        let matches: i64 = conn.query_row(
            "SELECT count(*) FROM social_publish_prepared WHERE install_id=? AND context_id IS ? AND run_id=? AND mode=? AND state IN ('prepared','authorized') AND (?='now' OR due_epoch=?)",
            params![install_id,context_id,run_id,mode,mode,due_epoch.unwrap_or_default()],
            |r| r.get(0),
        )?;
        if matches > 1 {
            return Err(Error::rejected(
                "multiple active prepared intents match this run scope",
            ));
        }
        let id: Option<String> = if matches == 0 {
            None
        } else {
            Some(conn.query_row(
                "SELECT prepared_id FROM social_publish_prepared WHERE install_id=? AND context_id IS ? AND run_id=? AND mode=? AND state IN ('prepared','authorized') AND (?='now' OR due_epoch=?)",
                params![install_id,context_id,run_id,mode,mode,due_epoch.unwrap_or_default()],
                |r| r.get(0),
            )?)
        };
        id.map(|id| read_prepared_row(&conn, &id)).transpose()
    }

    /// The current immutable intent addressed by an AOS-signed read
    /// assertion. This deliberately omits caller-supplied scope: the
    /// verifier already checked the configured issuer, workspace, audience,
    /// intent id and one-use jti. Terminal rows are never re-disclosed.
    pub(crate) fn social_publish_prepared_owner_show(&self, prepared_id: &str) -> Result<Value> {
        let conn = self.conn();
        let state: Option<String> = conn
            .query_row(
                "SELECT state FROM social_publish_prepared WHERE prepared_id=?",
                [prepared_id],
                |row| row.get(0),
            )
            .optional()?;
        if !matches!(state.as_deref(), Some("prepared" | "authorized")) {
            return Err(Error::rejected("prepared owner intent is unavailable"));
        }
        let media_key: Option<String> = conn.query_row(
            "SELECT media_key FROM social_publish_prepared WHERE prepared_id=?",
            [prepared_id],
            |row| row.get(0),
        )?;
        let mut shown = read_prepared_row(&conn, prepared_id)?;
        // This private daemon-only projection is consumed for local custody
        // re-proof and is never included in the HTTP descriptor response.
        shown["prepared"]["media_key"] = json!(media_key);
        Ok(shown)
    }

    /// The PREPARED intent by id, scoped to its own install and exact
    /// context (null-preserving). Out-of-scope refuses.
    pub(crate) fn social_publish_prepared_show_scoped(
        &self,
        prepared_id: &str,
        install_id: &str,
        context_id: Option<&str>,
    ) -> Result<Value> {
        let conn = self.conn();
        let in_scope: Option<String> = conn
            .query_row(
                "SELECT prepared_id FROM social_publish_prepared WHERE prepared_id=? AND install_id=? AND context_id IS ?",
                params![prepared_id, install_id, context_id],
                |r| r.get(0),
            )
            .optional()?;
        if in_scope.is_none() {
            return Err(Error::rejected(
                "no prepared publish intent with this id in this install and context",
            ));
        }
        read_prepared_row(&conn, prepared_id)
    }

    /// Non-mutating, exact-scope projection for owner completion status. It
    /// deliberately omits the descriptor, receipt, grant and owner material.
    pub(crate) fn social_publish_prepared_status_scoped(
        &self,
        prepared_id: &str,
        install_id: &str,
        context_id: Option<&str>,
    ) -> Result<Value> {
        let conn = self.conn();
        prepared_status_output(&conn, prepared_id, install_id, context_id)
    }

    /// Terminal, exact-scope cancellation. PREPARED rows become cancelled;
    /// an attached queue row can be cancelled only while still queued, so a
    /// claim and cancellation serialize with the same writer transaction.
    pub(crate) fn social_publish_prepared_cancel_scoped(
        &self,
        prepared_id: &str,
        install_id: &str,
        context_id: Option<&str>,
    ) -> Result<Value> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let prepared: Option<(String, String)> = tx
                .query_row_raw(
                    "SELECT request,state FROM social_publish_prepared WHERE prepared_id=? AND install_id=? AND context_id IS ?",
                    params![prepared_id,install_id,context_id],
                    |r| Ok((r.get(0)?,r.get(1)?)),
                )
                .optional()?;
            let (request, state) = prepared.ok_or_else(|| {
                Error::rejected("no prepared publish intent with this id in this install and context")
            })?;
            match state.as_str() {
                "cancelled" => return prepared_status_output(&tx, prepared_id, install_id, context_id),
                "prepared" => {
                    let has_queue: bool = tx.query_row_raw(
                        "SELECT EXISTS(SELECT 1 FROM social_publish_intents WHERE request=?)",
                        [&request],
                        |r| r.get(0),
                    )?;
                    if has_queue {
                        return Err(Error::rejected("prepared intent unexpectedly has queued-send state"));
                    }
                    let changed = tx.execute(
                        "UPDATE social_publish_prepared SET state='cancelled',updated=? WHERE prepared_id=? AND state='prepared' AND install_id=? AND context_id IS ?",
                        params![now(),prepared_id,install_id,context_id],
                    )?;
                    if changed != 1 {
                        return Err(Error::rejected("prepared intent changed before cancellation"));
                    }
                    Self::event(
                        &tx,
                        platform::PLATFORM_STREAM,
                        SOCIAL_PUBLISH_PREPARED_CANCELLED_EVENT,
                        json!({"prepared_id":prepared_id}),
                    )?;
                }
                "authorized" => {
                    let queued: Option<(String, String)> = tx
                        .query_row_raw(
                            "SELECT intent_id,state FROM social_publish_intents WHERE request=? AND install_id=? AND context_id IS ?",
                            params![request,install_id,context_id],
                            |r| Ok((r.get(0)?,r.get(1)?)),
                        )
                        .optional()?;
                    let (intent_id, queue_state) = queued.ok_or_else(|| {
                        Error::rejected("authorized prepared intent has no queued row in this scope")
                    })?;
                    if queue_state != "queued" {
                        return Err(Error::rejected("only an unclaimed queued owner attachment can be cancelled"));
                    }
                    let changed_queue = tx.execute(
                        "UPDATE social_publish_intents SET state='cancelled',updated=? WHERE intent_id=? AND state='queued' AND install_id=? AND context_id IS ?",
                        params![now(),intent_id,install_id,context_id],
                    )?;
                    let changed_prepared = tx.execute(
                        "UPDATE social_publish_prepared SET state='cancelled',updated=? WHERE prepared_id=? AND state='authorized' AND install_id=? AND context_id IS ?",
                        params![now(),prepared_id,install_id,context_id],
                    )?;
                    if changed_queue != 1 || changed_prepared != 1 {
                        return Err(Error::rejected("attached owner intent changed before cancellation"));
                    }
                    Self::event(
                        &tx,
                        platform::PLATFORM_STREAM,
                        SOCIAL_PUBLISH_CANCELLED_EVENT,
                        json!({"intent_id":intent_id}),
                    )?;
                    Self::event(
                        &tx,
                        platform::PLATFORM_STREAM,
                        SOCIAL_PUBLISH_PREPARED_CANCELLED_EVENT,
                        json!({"prepared_id":prepared_id,"queued_intent_id":intent_id}),
                    )?;
                }
                _ => return Err(Error::rejected("only an active prepared owner intent can be cancelled")),
            }
            prepared_status_output(&tx, prepared_id, install_id, context_id)
        })
    }

    /// Read-only retry path for an already attached intent. A committed
    /// attach always has its queued-send row in the same transaction; a
    /// historical or corrupt AUTHORIZED marker without that row refuses.
    pub(crate) fn social_publish_attached_show_scoped(
        &self,
        prepared_id: &str,
        install_id: &str,
        context_id: Option<&str>,
    ) -> Result<Value> {
        let conn = self.conn();
        attached_output(&conn, prepared_id, install_id, context_id)
    }

    /// Register the acceptance-only attach barrier for this database. The
    /// hook has no transaction, database, or authorization access and exists
    /// only in the non-release `test-seam` build.
    #[cfg(feature = "test-seam")]
    pub fn set_social_publish_attach_test_hook(&self, hook: Option<SocialPublishAttachTestHook>) {
        let mut hooks = social_publish_attach_test_hooks()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(hook) = hook {
            hooks.insert(self.db_identity.clone(), hook);
        } else {
            hooks.remove(&self.db_identity);
        }
    }

    /// Arm an already committed owner-attachment row after its first
    /// transaction. The undo floor is sampled only after that commit; claim
    /// paths remain fenced by `claim_armed=0` until this update commits.
    fn social_publish_arm_attached_intent(
        &self,
        prepared_id: &str,
        expected_intent_id: &str,
        install_id: &str,
        context_id: Option<&str>,
        queue_clock: &dyn Fn() -> f64,
    ) -> Result<Value> {
        let conn = self.conn();
        let current: Option<(String, i64, String)> = conn
            .query_row(
                "SELECT p.state,q.claim_armed,q.state FROM social_publish_prepared p JOIN social_publish_intents q ON q.request=p.request WHERE p.prepared_id=? AND p.install_id=? AND p.context_id IS ? AND q.intent_id=? AND q.install_id=? AND q.context_id IS ?",
                params![prepared_id,install_id,context_id,expected_intent_id,install_id,context_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
            )
            .optional()?;
        let (prepared_state, armed, queue_state) = current.ok_or_else(|| {
            Error::rejected("no attached owner queue row with this id in this install and context")
        })?;
        if prepared_state != "authorized" {
            return Err(Error::rejected(
                "only an authorized owner attachment can be armed",
            ));
        }
        if armed == 1 {
            drop(conn);
            return self.social_publish_attached_show_scoped(prepared_id, install_id, context_id);
        }
        if armed != 0 || queue_state != "queued" {
            return Err(Error::rejected(
                "unarmed owner queue row is not safely claimable",
            ));
        }
        drop(conn);

        // This sample is necessarily after the prepared→authorized + queue
        // commit above. The whole-second ceiling preserves the five-second
        // floor; a slow arm transaction only makes the row older before its
        // claim fence opens, never younger than the committed queue row.
        let claim_after = (queue_clock() + SOCIAL_PUBLISH_UNDO_SECS as f64).ceil();
        if !claim_after.is_finite() || claim_after <= 0.0 || claim_after >= i64::MAX as f64 {
            return Err(Error::rejected("queue maturity time is invalid"));
        }
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let changed = tx.execute(
                "UPDATE social_publish_intents SET claim_after_epoch=?,claim_armed=1,updated=? WHERE intent_id=? AND request=(SELECT request FROM social_publish_prepared WHERE prepared_id=? AND state='authorized' AND install_id=? AND context_id IS ?) AND state='queued' AND claim_armed=0 AND install_id=? AND context_id IS ?",
                params![claim_after as i64,now(),expected_intent_id,prepared_id,install_id,context_id,install_id,context_id],
            )?;
            if changed == 0 {
                // A concurrent exact attach retry may have armed the same
                // row. Return it only if the committed local handoff is now
                // fully valid; cancellation or a claim race still refuses.
                let attached = attached_output(&tx, prepared_id, install_id, context_id)?;
                if attached["queued"]["intent_id"] != expected_intent_id {
                    return Err(Error::rejected(
                        "authorized prepared queue row changed before arm",
                    ));
                }
                return Ok(attached);
            }
            if changed != 1 {
                return Err(Error::rejected("owner queue arm changed more than one row"));
            }
            let attached = attached_output(&tx, prepared_id, install_id, context_id)?;
            if attached["queued"]["intent_id"] != expected_intent_id {
                return Err(Error::rejected(
                    "authorized prepared queue row changed during arm",
                ));
            }
            Ok(attached)
        })
    }

    /// Atomically CAS PREPARED -> AUTHORIZED and insert the exact frozen
    /// queue row unarmed. An already-authorized recovery accepts only its
    /// freshly verified matching grant and validates the same queued row; it
    /// never inserts a duplicate or repeats lifecycle events. A separate
    /// post-commit arm installs the undo floor before any claim.
    pub(crate) fn social_publish_attach_intent(
        &self,
        prepared_id: &str,
        install_id: &str,
        context_id: Option<&str>,
        grant: &crate::daemon::social_publish_queue::VerifiedPublishGrant,
        queue_clock: &dyn Fn() -> f64,
    ) -> Result<Value> {
        if grant.prepared_id() != prepared_id
            || !crate::platform::agenticos_external::publish::valid_digest(
                grant.owner_intent_digest(),
            )
        {
            return Err(Error::rejected(
                "verified owner grant is not bound to this prepared intent",
            ));
        }
        let grant_id = grant.grant_id();
        if !crate::platform::agenticos_external::publish::valid_grant_id(grant_id) {
            return Err(Error::rejected("verified owner grant id is malformed"));
        }
        #[cfg(feature = "test-seam")]
        let test_hook = social_publish_attach_test_hook(&self.db_identity);
        let (intent_id, _newly_attached) = self.write_tx(|conn| {
            let tx = &mut *conn;
            let current: Option<(String, String, Option<String>, String, String)> = tx
                .query_row_raw(
                    "SELECT request,state,grant_id,descriptor,descriptor_digest FROM social_publish_prepared WHERE prepared_id=? AND install_id=? AND context_id IS ?",
                    params![prepared_id, install_id, context_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?;
            let Some((request, state, attached, descriptor_text, descriptor_digest)) = current else {
                return Err(Error::rejected(
                    "no prepared publish intent with this id in this install and context",
                ));
            };
            if grant.prepared_descriptor_digest() != descriptor_digest {
                return Err(Error::rejected(
                    "verified owner grant names a changed prepared descriptor",
                ));
            }
            let descriptor: Value = serde_json::from_str(&descriptor_text)
                .map_err(|_| Error::rejected("prepared publish descriptor is corrupt"))?;
            if app_runs::material_digest(&descriptor) != descriptor_digest {
                return Err(Error::rejected("prepared intent descriptor receipt is corrupt"));
            }
            let mut send_row = prepared_send_row(&request, &descriptor, grant_id, 0)?;
            validate_new(&send_row)?;
            crate::proto::identifier(&request, "prepared publish request key")?;
            if !crate::platform::agenticos_external::publish::valid_idempotency_key(&request) {
                return Err(Error::rejected("prepared publish request key is malformed"));
            }

            match state.as_str() {
                "authorized" if attached.as_deref() == Some(grant_id) => {
                    let intent_id = authorized_queue_recovery_id(
                        tx,
                        prepared_id,
                        install_id,
                        context_id,
                        &request,
                        grant_id,
                        &descriptor,
                    )?;
                    return Ok((intent_id, false));
                }
                "authorized" => {
                    return Err(Error::rejected(
                        "this prepared intent already authorized a different grant",
                    ));
                }
                "prepared" => {}
                _ => {
                    return Err(Error::rejected(
                        "only a prepared publish intent can accept an owner grant",
                    ));
                }
            }
            let queued: Option<String> = tx
                .query_row_raw(
                    "SELECT intent_id FROM social_publish_intents WHERE request=?",
                    [&request],
                    |r| r.get(0),
                )
                .optional()?;
            if queued.is_some() {
                return Err(Error::rejected(
                    "prepared publish request already has queued-send state",
                ));
            }
            let replayed: Option<String> = tx
                .query_row_raw(
                    "SELECT intent_id FROM social_publish_intents WHERE approval_id=?",
                    [send_row.approval_id],
                    |r| r.get(0),
                )
                .optional()?;
            if replayed.is_some() {
                return Err(Error::rejected(
                    "approval_replay: this approval already authorized another social publish intent",
                ));
            }

            // The prepared→authorized marker, unarmed queue insertion and
            // lifecycle events share this writer transaction. The queue row
            // cannot be claimed until a later transaction arms it after this
            // commit, so this commit establishes the maturity-floor origin.
            let frozen = frozen_of(&send_row);
            let frozen_digest = app_runs::material_digest(&frozen);

            let changed = tx.execute(
                "UPDATE social_publish_prepared SET state='authorized',grant_id=?,updated=? WHERE prepared_id=? AND state='prepared' AND install_id=? AND context_id IS ? AND descriptor_digest=?",
                params![grant_id, now(), prepared_id, install_id, context_id, descriptor_digest],
            )?;
            if changed != 1 {
                return Err(Error::rejected(
                    "prepared intent left the prepared state before the grant attached",
                ));
            }
            let intent_id = format!("spub-{}", uuid::Uuid::new_v4().simple());
            let created = now();
            send_row.claim_after_epoch = 0;
            validate_new(&send_row)?;
            tx.execute(
                "INSERT INTO social_publish_intents(intent_id,request,install_id,context_id,run_id,effect_id,connection_id,destination_id,toolkit,caption_digest,image_digest,media_key,grant_id,approval_id,due_epoch,claim_after_epoch,claim_armed,timezone,state,frozen,frozen_digest,created,updated) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,0,?,'queued',?,?,?,?)",
                params![intent_id,request,send_row.install_id,send_row.context_id,send_row.run_id,send_row.effect_id,send_row.connection_id,send_row.destination_id,send_row.toolkit,send_row.caption_digest,send_row.image_digest,send_row.media_key,grant_id,send_row.approval_id,send_row.due_epoch,send_row.claim_after_epoch,send_row.timezone,frozen.to_string(),frozen_digest,created,created],
            )?;
            Self::event(
                &tx,
                platform::PLATFORM_STREAM,
                SOCIAL_PUBLISH_SCHEDULED_EVENT,
                json!({"intent_id":intent_id.clone(),"request":request,"digest":frozen_digest}),
            )?;
            Self::event(
                &tx,
                platform::PLATFORM_STREAM,
                SOCIAL_PUBLISH_AUTHORIZED_EVENT,
                json!({
                    "prepared_id":prepared_id,
                    "owner_intent_digest":grant.owner_intent_digest(),
                    "queued_intent_id":intent_id.clone(),
                }),
            )?;
            #[cfg(feature = "test-seam")]
            if let Some(hook) = test_hook.as_ref() {
                hook(SocialPublishAttachBoundary::BeforeAuthorizationCommit);
            }
            Ok((intent_id, true))
        })?;
        #[cfg(feature = "test-seam")]
        if _newly_attached {
            if let Some(hook) = test_hook.as_ref() {
                hook(SocialPublishAttachBoundary::AfterAuthorizationCommitBeforeArm);
            }
        }
        self.social_publish_arm_attached_intent(
            prepared_id,
            &intent_id,
            install_id,
            context_id,
            queue_clock,
        )
    }
}
