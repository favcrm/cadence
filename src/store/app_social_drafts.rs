//! Typed social drafts in the installation-witnessed RecordStore file.
//! This family is deliberately separate from customer/email records.
use super::app_records::RecordStore;
use super::*;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const DRAFT_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS app_social_drafts(\
 install_id TEXT NOT NULL, context_id TEXT NOT NULL, draft_id TEXT NOT NULL,\
 revision INTEGER NOT NULL CHECK(revision>0), caption TEXT NOT NULL,\
 source_json TEXT NOT NULL, asset_id TEXT, created REAL NOT NULL, updated REAL NOT NULL,\
 discarded_at REAL,\
 PRIMARY KEY(context_id,draft_id));\
CREATE INDEX IF NOT EXISTS app_social_drafts_context ON app_social_drafts(context_id,updated,draft_id);\
CREATE TABLE IF NOT EXISTS app_social_draft_revisions(\
 install_id TEXT NOT NULL, context_id TEXT NOT NULL, draft_id TEXT NOT NULL,\
 revision INTEGER NOT NULL CHECK(revision>0), caption TEXT NOT NULL,\
 source_json TEXT NOT NULL, asset_id TEXT, actor TEXT NOT NULL, at REAL NOT NULL,\
 PRIMARY KEY(context_id,draft_id,revision));\
CREATE TABLE IF NOT EXISTS app_social_draft_requests(\
 request_id TEXT PRIMARY KEY, install_id TEXT NOT NULL, context_id TEXT NOT NULL,\
 draft_id TEXT NOT NULL, intent_digest TEXT NOT NULL, response_json TEXT NOT NULL, at REAL NOT NULL);\
CREATE TABLE IF NOT EXISTS app_social_sources(\
 install_id TEXT NOT NULL, context_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),\
 handles_json TEXT NOT NULL, updated REAL NOT NULL, PRIMARY KEY(context_id));\
CREATE TABLE IF NOT EXISTS app_social_source_requests(\
 request_id TEXT PRIMARY KEY, install_id TEXT NOT NULL, context_id TEXT NOT NULL,\
 intent_digest TEXT NOT NULL, at REAL NOT NULL);\
CREATE TABLE IF NOT EXISTS app_social_tool_receipts(\
 install_id TEXT NOT NULL, context_id TEXT NOT NULL, receipt_id TEXT NOT NULL, completed_at REAL NOT NULL,\
 PRIMARY KEY(context_id,receipt_id));\
CREATE TABLE IF NOT EXISTS app_social_freshness(\
 install_id TEXT NOT NULL, context_id TEXT NOT NULL, handle TEXT NOT NULL, receipt_id TEXT NOT NULL, completed_at REAL NOT NULL,\
 PRIMARY KEY(context_id,handle));\
CREATE TABLE IF NOT EXISTS app_social_generation_intents(\
 intent_digest TEXT NOT NULL, install_id TEXT NOT NULL, context_id TEXT NOT NULL, alias TEXT NOT NULL, tool TEXT NOT NULL, caller_input_digest TEXT NOT NULL DEFAULT '', input_digest TEXT NOT NULL, input_json TEXT NOT NULL, scope_json TEXT NOT NULL, request_id TEXT NOT NULL UNIQUE,\
 state TEXT NOT NULL CHECK(state IN ('pending','uncertain','completed','refused')), receipt_id TEXT, outcome TEXT, updated REAL NOT NULL,\
 PRIMARY KEY(context_id,intent_digest));\
CREATE TABLE IF NOT EXISTS app_social_image_jobs(\
 call_id TEXT PRIMARY KEY, install_id TEXT NOT NULL, context_id TEXT NOT NULL,\
 intent_digest TEXT NOT NULL, request_id TEXT NOT NULL UNIQUE,\
 state TEXT NOT NULL CHECK(state IN ('active','succeeded','uncertain','failed')),\
 reason TEXT, spec_json TEXT, created REAL NOT NULL, deadline REAL NOT NULL, updated REAL NOT NULL);\
CREATE TABLE IF NOT EXISTS app_social_effects(\
 effect_id TEXT PRIMARY KEY, install_id TEXT NOT NULL, context_id TEXT NOT NULL,\
 draft_id TEXT NOT NULL, revision INTEGER NOT NULL, request_id TEXT NOT NULL UNIQUE,\
 digest TEXT NOT NULL, frozen_json TEXT NOT NULL, state TEXT NOT NULL \
 CHECK(state IN ('waiting','approved','declined','sending','posted','refused')) ,\
 approval_id TEXT NOT NULL, outcome_json TEXT, media_key TEXT, created REAL NOT NULL, updated REAL NOT NULL);";

pub const CAPTION_MAX_SCALARS: usize =
    crate::platform::agenticos_external::publish::DEVICE_PUBLISH_MAX_CAPTION_SCALARS;

/// Arguments of one draft edit (a new revision of the same draft).
#[derive(Clone, Copy)]
pub struct SocialDraftEdit<'a> {
    pub expected: i64,
    pub caption: &'a str,
    pub asset_id: Option<Option<&'a str>>,
    pub request_id: &'a str,
    pub actor: &'a str,
}

/// Arguments of one scoped generation intent.
#[derive(Clone, Copy)]
pub struct GenerationStart<'a> {
    pub alias: &'a str,
    pub tool: &'a str,
    pub caller_input_digest: &'a str,
    pub input: &'a Value,
    pub scope: &'a Value,
    pub request: &'a str,
}

/// CAD-1315: one standalone image job (the AgenticOS call behind an intent).
/// `spec` is the frozen authority the worker replays under the same key; it is
/// dropped once the job is terminal.
#[derive(Clone, Debug)]
pub struct ImageJob {
    pub call_id: String,
    pub context_id: String,
    pub intent_digest: String,
    pub request_id: String,
    pub state: String,
    pub reason: Option<String>,
    pub spec: Option<Value>,
    pub deadline: f64,
}

/// How a worker ends an active image job.
#[derive(Clone, Copy)]
pub enum ImageSettlement<'a> {
    Succeeded { receipt: &'a str },
    Failed { reason: &'a str },
    Uncertain { reason: &'a str },
}

/// Arguments of one staged social draft effect.
#[derive(Clone, Copy)]
pub struct EffectStage<'a> {
    pub draft: &'a str,
    pub revision: i64,
    pub request: &'a str,
    pub effect_id: &'a str,
    pub frozen: &'a Value,
    pub approval: &'a str,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DraftSource {
    ToolReceipt {
        receipt_id: String,
        post_id: Option<String>,
    },
    Run {
        run_id: String,
        receipt_id: String,
        post_id: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocialDraft {
    pub draft_id: String,
    pub revision: i64,
    pub caption: String,
    pub source: DraftSource,
    pub asset_id: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
}

fn valid_caption(caption: &str) -> Result<()> {
    if caption.is_empty() || caption.chars().count() > CAPTION_MAX_SCALARS || caption.contains('\0')
    {
        return Err(Error::rejected(
            "social caption is empty or exceeds the publish limit",
        ));
    }
    Ok(())
}

pub(crate) fn ensure_schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(DRAFT_SCHEMA)
        .map_err(|e| Error::internal(format!("social draft schema unavailable: {e}")))?;
    let columns = conn
        .prepare("PRAGMA table_info(app_social_effects)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|c| c == "media_key") {
        conn.execute(
            "ALTER TABLE app_social_effects ADD COLUMN media_key TEXT",
            [],
        )?;
    }
    // CAD-1303: soft discard marker. Additive; the file schema version stays.
    let draft_columns = conn
        .prepare("PRAGMA table_info(app_social_drafts)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !draft_columns.iter().any(|c| c == "discarded_at") {
        conn.execute(
            "ALTER TABLE app_social_drafts ADD COLUMN discarded_at REAL",
            [],
        )?;
    }
    let generation_columns = conn
        .prepare("PRAGMA table_info(app_social_generation_intents)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !generation_columns.iter().any(|c| c == "input_json") {
        conn.execute("ALTER TABLE app_social_generation_intents ADD COLUMN input_json TEXT NOT NULL DEFAULT '{}'",[])?;
    }
    if !generation_columns
        .iter()
        .any(|c| c == "caller_input_digest")
    {
        conn.execute("ALTER TABLE app_social_generation_intents ADD COLUMN caller_input_digest TEXT NOT NULL DEFAULT ''",[])?;
        // Before frozen plans existed, input_digest was the caller-input digest.
        // Only rows from that pre-column schema can be migrated this way.
        conn.execute("UPDATE app_social_generation_intents SET caller_input_digest=input_digest WHERE caller_input_digest=''",[])?;
    }
    Ok(())
}

impl RecordStore {
    fn social_draft_in(
        conn: &impl StoreConn,
        install: &str,
        context: &str,
        id: &str,
    ) -> Result<Value> {
        let row = conn.query_row(
            "SELECT draft_id,revision,caption,source_json,asset_id,created,updated FROM app_social_drafts WHERE install_id=? AND context_id=? AND draft_id=? AND discarded_at IS NULL",
            params![install, context, id],
            |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,f64>(5)?,r.get::<_,f64>(6)?)),
        ).optional()?.ok_or_else(|| Error::rejected("social draft not found in this context"))?;
        let source: DraftSource = serde_json::from_str(&row.3)
            .map_err(|_| Error::rejected("social draft provenance is corrupt"))?;
        Ok(
            json!({"draft_id":row.0,"revision":row.1,"caption":row.2,"source":source,"asset_id":row.4,"created_at":row.5,"updated_at":row.6}),
        )
    }

    pub fn app_social_draft_show(&self, context: &str, id: &str) -> Result<Value> {
        Self::social_draft_in(&*self.conn(), self.install(), context, id)
    }

    pub fn app_social_draft_list(&self, context: &str) -> Result<Value> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT draft_id FROM app_social_drafts WHERE install_id=? AND context_id=? AND discarded_at IS NULL ORDER BY updated DESC,draft_id LIMIT 101")?;
        let ids = stmt
            .query_map(params![self.install(), context], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if ids.len() > 100 {
            return Err(Error::rejected(
                "social draft inventory exceeds its supported bound",
            ));
        }
        Ok(
            json!({"drafts":ids.iter().map(|id| Self::social_draft_in(&*conn,self.install(),context,id)).collect::<Result<Vec<_>>>()?}),
        )
    }

    pub fn app_social_draft_create(
        &self,
        context: &str,
        caption: &str,
        source: &DraftSource,
        asset_id: Option<&str>,
        request_id: &str,
        actor: &str,
    ) -> Result<Value> {
        valid_caption(caption)?;
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(request_id, "social draft request ID")?;
        if let Some(id) = asset_id {
            crate::proto::identifier(id, "social draft asset ID")?;
        }
        let source_json =
            serde_json::to_string(source).map_err(|e| Error::internal(e.to_string()))?;
        let intent = crate::store::app_runs::material_digest(
            &json!({"context":context,"caption":caption,"source":source,"asset_id":asset_id}),
        );
        self.write_tx(|tx| {
            if let Some((saved,))=tx.query_row("SELECT intent_digest FROM app_social_draft_requests WHERE request_id=?",[request_id],|r|Ok((r.get::<_,String>(0)?,))).optional()? {
                if saved!=intent { return Err(Error::rejected("social draft request ID was reused for a different intent")); }
                let id:String=tx.query_row("SELECT draft_id FROM app_social_draft_requests WHERE request_id=?",[request_id],|r|r.get(0))?;
                return Self::social_draft_in(tx,self.install(),context,&id);
            }
            let id=format!("sdr-{}",uuid::Uuid::new_v4().simple()); let at=now();
            tx.execute("INSERT INTO app_social_drafts(install_id,context_id,draft_id,revision,caption,source_json,asset_id,created,updated) VALUES(?,?,?,1,?,?,?,?,?)",params![self.install(),context,id,caption,source_json,asset_id,at,at])?;
            tx.execute("INSERT INTO app_social_draft_revisions(install_id,context_id,draft_id,revision,caption,source_json,asset_id,actor,at) VALUES(?,?,?,1,?,?,?,?,?)",params![self.install(),context,id,caption,source_json,asset_id,actor,at])?;
            tx.execute("INSERT INTO app_social_draft_requests(request_id,install_id,context_id,draft_id,intent_digest,response_json,at) VALUES(?,?,?,?,?,?,?)",params![request_id,self.install(),context,id,intent,"{}",at])?;
            Self::social_draft_in(tx,self.install(),context,&id)
        })
    }

    pub fn app_social_draft_update(
        &self,
        context: &str,
        id: &str,
        edit: &SocialDraftEdit<'_>,
    ) -> Result<Value> {
        let SocialDraftEdit {
            expected,
            caption,
            asset_id,
            request_id,
            actor,
        } = *edit;
        valid_caption(caption)?;
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(id, "social draft ID")?;
        crate::proto::identifier(request_id, "social draft request ID")?;
        if let Some(Some(asset)) = asset_id {
            crate::proto::identifier(asset, "social draft asset ID")?;
        }
        if expected <= 0 {
            return Err(Error::rejected(
                "expected social draft revision must be positive",
            ));
        }
        let intent = crate::store::app_runs::material_digest(
            &json!({"context":context,"draft_id":id,"expected_revision":expected,"caption":caption,"asset_id":asset_id}),
        );
        self.write_tx(|tx| {
            if let Some((saved,draft))=tx.query_row("SELECT intent_digest,draft_id FROM app_social_draft_requests WHERE request_id=?",[request_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()? {
                if saved!=intent || draft!=id { return Err(Error::rejected("social draft request ID was reused for a different intent")); }
                return Self::social_draft_in(tx,self.install(),context,id);
            }
            let (revision,old_source,old_asset):(i64,String,Option<String>)=tx.query_row("SELECT revision,source_json,asset_id FROM app_social_drafts WHERE install_id=? AND context_id=? AND draft_id=? AND discarded_at IS NULL",params![self.install(),context,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or_else(||Error::rejected("social draft not found in this context"))?;
            if revision!=expected { return Err(Error::conflict(expected,"social draft revision is stale")); }
            let newasset=asset_id.unwrap_or(old_asset.as_deref()).map(str::to_owned); let next=revision+1; let at=now();
            tx.execute("UPDATE app_social_drafts SET revision=?,caption=?,asset_id=?,updated=? WHERE install_id=? AND context_id=? AND draft_id=? AND revision=?",params![next,caption,newasset,at,self.install(),context,id,expected])?;
            tx.execute("INSERT INTO app_social_draft_revisions(install_id,context_id,draft_id,revision,caption,source_json,asset_id,actor,at) VALUES(?,?,?,?,?,?,?,?,?)",params![self.install(),context,id,next,caption,old_source,newasset,actor,at])?;
            tx.execute("INSERT INTO app_social_draft_requests(request_id,install_id,context_id,draft_id,intent_digest,response_json,at) VALUES(?,?,?,?,?,?,?)",params![request_id,self.install(),context,id,intent,"{}",at])?;
            Self::social_draft_in(tx,self.install(),context,id)
        })
    }

    /// CAD-1303: soft discard. The record, its revisions and its effects stay
    /// for audit and cost history; it only leaves list/show/update. Refused
    /// while any effect of the draft is approved, sending or posted. Waiting
    /// effects are declined so a hidden draft can never be approved later.
    pub fn app_social_draft_discard(
        &self,
        context: &str,
        id: &str,
        revision: i64,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(id, "social draft ID")?;
        self.write_tx(|tx| {
            let current: Option<i64> = tx
                .query_row(
                    "SELECT revision FROM app_social_drafts WHERE install_id=? AND context_id=? AND draft_id=? AND discarded_at IS NULL",
                    params![self.install(), context, id],
                    |r| r.get(0),
                )
                .optional()?;
            let current = current.ok_or_else(|| {
                Error::invalid("draft_not_found", "That draft is no longer here.")
            })?;
            if current != revision {
                return Err(Error::Structured(crate::error::Structured {
                    kind: "conflict",
                    code: "stale_revision".into(),
                    message: "social draft revision is stale".into(),
                    revision: Some(current),
                }));
            }
            let live: i64 = tx.query_row(
                "SELECT COUNT(*) FROM app_social_effects WHERE install_id=? AND context_id=? AND draft_id=? AND state IN ('approved','sending','posted')",
                params![self.install(), context, id],
                |r| r.get(0),
            )?;
            if live > 0 {
                return Err(Error::gate_coded(
                    "draft_in_use",
                    "This draft is approved or published, so it cannot be discarded.",
                ));
            }
            let at = now();
            tx.execute(
                "UPDATE app_social_effects SET state='declined',updated=? WHERE install_id=? AND context_id=? AND draft_id=? AND state='waiting'",
                params![at, self.install(), context, id],
            )?;
            tx.execute(
                "UPDATE app_social_drafts SET discarded_at=? WHERE install_id=? AND context_id=? AND draft_id=? AND revision=? AND discarded_at IS NULL",
                params![at, self.install(), context, id, revision],
            )?;
            Ok(json!({"draft_id":id,"state":"discarded","revision":revision}))
        })
    }

    pub fn app_social_draft_revision(
        &self,
        context: &str,
        id: &str,
        revision: i64,
    ) -> Result<Value> {
        let conn = self.conn();
        let (caption,source,asset,at):(String,String,Option<String>,f64)=conn.query_row("SELECT caption,source_json,asset_id,at FROM app_social_draft_revisions WHERE install_id=? AND context_id=? AND draft_id=? AND revision=?",params![self.install(),context,id,revision],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.ok_or_else(||Error::rejected("social draft revision not found"))?;
        Ok(
            json!({"draft_id":id,"context_id":context,"install_id":self.install(),"revision":revision,"caption":caption,"source":serde_json::from_str::<DraftSource>(&source).map_err(|_|Error::rejected("social draft provenance is corrupt"))?,"asset_id":asset,"at":at}),
        )
    }

    pub fn app_social_freshness_save(
        &self,
        context: &str,
        handle: &str,
        receipt: &str,
        completed: f64,
    ) -> Result<()> {
        let current = self.app_social_sources_show(context)?;
        if !current["handles"]
            .as_array()
            .is_some_and(|hs| hs.iter().any(|h| h.as_str() == Some(handle)))
        {
            return Err(Error::rejected(
                "source handle is not saved in this context",
            ));
        }
        self.write_tx(|tx| {
            tx.execute("INSERT INTO app_social_freshness(install_id,context_id,handle,receipt_id,completed_at) VALUES(?,?,?,?,?) ON CONFLICT(context_id,handle) DO UPDATE SET receipt_id=excluded.receipt_id,completed_at=excluded.completed_at WHERE excluded.completed_at>app_social_freshness.completed_at OR (excluded.completed_at=app_social_freshness.completed_at AND excluded.receipt_id>app_social_freshness.receipt_id)", params![self.install(), context, handle, receipt, completed])?;
            Ok(())
        })
    }

    /// Positional form of [`Self::app_social_generation_begin_with`], kept
    /// for the independently owned acceptance check's call shape.
    #[allow(clippy::too_many_arguments)]
    pub fn app_social_generation_begin(
        &self,
        context: &str,
        intent: &str,
        alias: &str,
        tool: &str,
        caller_input_digest: &str,
        input: &Value,
        scope: &Value,
        request: &str,
    ) -> Result<Value> {
        self.app_social_generation_begin_with(
            context,
            intent,
            &GenerationStart {
                alias,
                tool,
                caller_input_digest,
                input,
                scope,
                request,
            },
        )
    }

    pub fn app_social_generation_begin_with(
        &self,
        context: &str,
        intent: &str,
        generation: &GenerationStart<'_>,
    ) -> Result<Value> {
        let GenerationStart {
            alias,
            tool,
            caller_input_digest,
            input,
            scope,
            request,
        } = *generation;
        let packed = serde_json::to_string(scope).map_err(|e| Error::internal(e.to_string()))?;
        let input_json =
            serde_json::to_string(input).map_err(|e| Error::internal(e.to_string()))?;
        self.write_tx(|tx|{
            let old=tx.query_row("SELECT request_id,state,receipt_id,outcome,updated,caller_input_digest,input_digest,input_json FROM app_social_generation_intents WHERE install_id=? AND context_id=? AND intent_digest=?",params![self.install(),context,intent],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,f64>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?))).optional()?;
            if let Some((old_request,state,receipt,outcome,updated,old_caller_digest,old_plan_digest,old_json))=old {
                if matches!(state.as_str(),"pending"|"uncertain") {
                    if old_caller_digest!=caller_input_digest{return Err(Error::rejected("generation intent is unresolved; changed caller input requires terminal settlement first"));}
                    return Ok(json!({"mode":if old_request==request{"retry"}else{"existing"},"intent":{"request_id":old_request,"alias":alias,"tool":tool,"caller_input_digest":old_caller_digest,"input_digest":old_plan_digest,"input":serde_json::from_str::<Value>(&old_json).map_err(|_|Error::rejected("generation input is corrupt"))?,"scope":scope,"state":state,"receipt_id":receipt,"outcome":outcome,"updated_at":updated}}));
                }
                if old_request==request{return Ok(json!({"mode":"terminal","intent":{"request_id":old_request,"state":state,"receipt_id":receipt,"outcome":outcome,"updated_at":updated}}));}
                tx.execute("UPDATE app_social_generation_intents SET alias=?,tool=?,caller_input_digest=?,input_digest=?,input_json=?,scope_json=?,request_id=?,state='pending',receipt_id=NULL,outcome=NULL,updated=? WHERE install_id=? AND context_id=? AND intent_digest=?",params![alias,tool,caller_input_digest,crate::store::app_runs::material_digest(input),input_json,packed,request,now(),self.install(),context,intent])?;
                return Ok(json!({"mode":"start","intent":{"request_id":request,"state":"pending"}}));
            }
            tx.execute("INSERT INTO app_social_generation_intents(intent_digest,install_id,context_id,alias,tool,caller_input_digest,input_digest,input_json,scope_json,request_id,state,updated) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",params![intent,self.install(),context,alias,tool,caller_input_digest,crate::store::app_runs::material_digest(input),input_json,packed,request,"pending",now()])?;
            Ok(json!({"mode":"start","intent":{"request_id":request,"state":"pending"}}))
        })
    }
    pub fn app_social_generation_finish(
        &self,
        context: &str,
        intent: &str,
        request: &str,
        state: &str,
        receipt: Option<&str>,
        outcome: Option<&str>,
    ) -> Result<()> {
        if !matches!(state, "pending" | "uncertain" | "completed" | "refused") {
            return Err(Error::rejected(
                "generation intent outcome state is invalid",
            ));
        }
        self.write_tx(|tx|{let changed=tx.execute("UPDATE app_social_generation_intents SET state=?,receipt_id=?,outcome=?,updated=? WHERE install_id=? AND context_id=? AND intent_digest=? AND request_id=? AND state IN ('pending','uncertain')",params![state,receipt,outcome,now(),self.install(),context,intent,request])?;if changed!=1{return Err(Error::rejected("generation intent key changed"));}Ok(())})
    }
    /// CAD-1315: start the one job behind a pending image intent. `call_id`
    /// is the AgenticOS idempotency key; a repeat is a no-op, so a job exists
    /// exactly once per key. The intent leaves `uncertain` for `pending` in
    /// the same transaction, and only for the request that owns it.
    pub fn app_social_image_job_start(
        &self,
        context: &str,
        intent: &str,
        request: &str,
        call_id: &str,
        spec: &Value,
        deadline: f64,
    ) -> Result<()> {
        let spec = serde_json::to_string(spec).map_err(|e| Error::internal(e.to_string()))?;
        self.write_tx(|tx| {
            let at = now();
            tx.execute("INSERT OR IGNORE INTO app_social_image_jobs(call_id,install_id,context_id,intent_digest,request_id,state,spec_json,created,deadline,updated) VALUES(?,?,?,?,?,'active',?,?,?,?)", params![call_id, self.install(), context, intent, request, spec, at, deadline, at])?;
            let changed = tx.execute("UPDATE app_social_generation_intents SET state='pending',outcome=NULL,updated=? WHERE install_id=? AND context_id=? AND intent_digest=? AND request_id=? AND state IN ('pending','uncertain')", params![at, self.install(), context, intent, request])?;
            if changed != 1 {
                return Err(Error::rejected("generation intent key changed"));
            }
            Ok(())
        })
    }

    fn image_job_in(conn: &impl StoreConn, filter: &str, key: &str) -> Result<Option<ImageJob>> {
        let row = conn.query_row(&format!("SELECT call_id,context_id,intent_digest,request_id,state,reason,spec_json,deadline FROM app_social_image_jobs WHERE {filter}=?"), [key], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,f64>(7)?))).optional()?;
        row.map(|r| {
            let spec = match r.6 {
                Some(text) => Some(
                    serde_json::from_str(&text)
                        .map_err(|_| Error::rejected("image job spec is corrupt"))?,
                ),
                None => None,
            };
            Ok(ImageJob {
                call_id: r.0,
                context_id: r.1,
                intent_digest: r.2,
                request_id: r.3,
                state: r.4,
                reason: r.5,
                spec,
                deadline: r.7,
            })
        })
        .transpose()
    }

    pub fn app_social_image_job_for_request(&self, request: &str) -> Result<Option<ImageJob>> {
        Self::image_job_in(&*self.conn(), "request_id", request)
    }

    pub fn app_social_image_job(&self, call_id: &str) -> Result<Option<ImageJob>> {
        Self::image_job_in(&*self.conn(), "call_id", call_id)
    }

    /// Call ids of every job a restart must drive to a terminal state.
    pub fn app_social_image_jobs_active(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let rows = conn
            .prepare("SELECT call_id FROM app_social_image_jobs WHERE install_id=? AND state='active' ORDER BY created")?
            .query_map([self.install()], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Re-check an unresolved job under its SAME key: `uncertain` returns to
    /// `active` (a new window), its intent to `pending`. Never a new key.
    pub fn app_social_image_job_resume(&self, call_id: &str, deadline: f64) -> Result<bool> {
        self.write_tx(|tx| {
            let at = now();
            let changed = tx.execute("UPDATE app_social_image_jobs SET state='active',reason=NULL,deadline=?,updated=? WHERE call_id=? AND install_id=? AND state='uncertain'", params![deadline, at, call_id, self.install()])?;
            if changed == 1 {
                tx.execute("UPDATE app_social_generation_intents SET state='pending',outcome=NULL,updated=? WHERE install_id=? AND state='uncertain' AND request_id=(SELECT request_id FROM app_social_image_jobs WHERE call_id=?)", params![at, self.install(), call_id])?;
            }
            Ok(changed == 1)
        })
    }

    /// Settle an active job and its intent together. The job row is the
    /// compare-and-set: a second settle (a replayed worker) changes nothing.
    pub fn app_social_image_job_settle(
        &self,
        call_id: &str,
        settlement: ImageSettlement<'_>,
    ) -> Result<bool> {
        let (job_state, intent_state, receipt, reason) = match settlement {
            ImageSettlement::Succeeded { receipt } => {
                ("succeeded", "completed", Some(receipt), None)
            }
            ImageSettlement::Failed { reason } => ("failed", "refused", None, Some(reason)),
            ImageSettlement::Uncertain { reason } => ("uncertain", "uncertain", None, Some(reason)),
        };
        self.write_tx(|tx| {
            let at = now();
            let keep_spec = job_state == "uncertain";
            let changed = tx.execute(&format!("UPDATE app_social_image_jobs SET state=?,reason=?,{}updated=? WHERE call_id=? AND install_id=? AND state='active'", if keep_spec { "" } else { "spec_json=NULL," }), params![job_state, reason, at, call_id, self.install()])?;
            if changed == 1 {
                tx.execute("UPDATE app_social_generation_intents SET state=?,receipt_id=?,outcome=?,updated=? WHERE install_id=? AND state IN ('pending','uncertain') AND request_id=(SELECT request_id FROM app_social_image_jobs WHERE call_id=?)", params![intent_state, receipt, reason, at, self.install(), call_id])?;
            }
            Ok(changed == 1)
        })
    }

    pub fn app_social_generation_complete_request(
        &self,
        request: &str,
        receipt: &str,
    ) -> Result<()> {
        self.write_tx(|tx|{tx.execute("UPDATE app_social_generation_intents SET state='completed',receipt_id=?,outcome=NULL,updated=? WHERE install_id=? AND request_id=? AND state IN ('pending','uncertain')",params![receipt,now(),self.install(),request])?;Ok(())})
    }
    pub fn app_social_generation_validate_receipt(
        &self,
        context: &str,
        request: &str,
        receipt: &Value,
    ) -> Result<()> {
        let intent = self.conn().query_row(
            "SELECT alias,caller_input_digest,input_digest,input_json,scope_json,state,receipt_id FROM app_social_generation_intents WHERE install_id=? AND context_id=? AND request_id=?",
            params![self.install(), context, request],
            |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,Option<String>>(6)?)),
        ).optional()?.ok_or_else(|| Error::rejected("generation intent is not scoped to this context"))?;
        let plan: Value = serde_json::from_str(&intent.3)
            .map_err(|_| Error::rejected("generation input is corrupt"))?;
        let _scope: Value = serde_json::from_str(&intent.4)
            .map_err(|_| Error::rejected("generation intent scope is corrupt"))?;
        if crate::store::app_runs::material_digest(&plan) != intent.2
            || intent.1.is_empty()
            || receipt["install_id"] != self.install()
            || receipt["request_id"] != request
            || receipt["alias"] != intent.0
            || receipt["input_digest"] != intent.1
            || (intent.5 == "completed" && intent.6.as_deref() != receipt["id"].as_str())
        {
            return Err(Error::rejected(
                "generation receipt no longer matches its scoped intent",
            ));
        }
        Ok(())
    }

    pub fn app_social_generation_intents(&self, context: &str) -> Result<Vec<Value>> {
        let conn = self.conn();
        let rows=conn.prepare("SELECT alias,tool,input_digest,input_json,scope_json,request_id,state,receipt_id,outcome,updated FROM app_social_generation_intents WHERE install_id=? AND context_id=? ORDER BY CASE WHEN state IN ('pending','uncertain') THEN 0 ELSE 1 END,updated DESC LIMIT 16")?.query_map(params![self.install(),context],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,f64>(9)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(|r|Ok(json!({"alias":r.0,"tool":r.1,"input_digest":r.2,"input":serde_json::from_str::<Value>(&r.3).map_err(|_|Error::rejected("generation input is corrupt"))?,"scope":serde_json::from_str::<Value>(&r.4).map_err(|_|Error::rejected("generation intent scope is corrupt"))?,"request_id":r.5,"state":r.6,"receipt_id":r.7,"outcome":r.8,"updated_at":r.9}))).collect()
    }

    pub fn app_social_tool_receipt_attach(
        &self,
        context: &str,
        receipt: &str,
        completed: f64,
    ) -> Result<()> {
        self.write_tx(|tx|{tx.execute("INSERT OR IGNORE INTO app_social_tool_receipts(install_id,context_id,receipt_id,completed_at) VALUES(?,?,?,?)",params![self.install(),context,receipt,completed])?;Ok(())})
    }
    pub fn app_social_tool_receipt_contains(&self, context: &str, receipt: &str) -> Result<bool> {
        Ok(self.conn().query_row("SELECT EXISTS(SELECT 1 FROM app_social_tool_receipts WHERE install_id=? AND context_id=? AND receipt_id=?)",params![self.install(),context,receipt],|r|r.get(0))?)
    }
    pub fn app_social_freshness_contains(&self, context: &str, receipt: &str) -> Result<bool> {
        Ok(self.conn().query_row("SELECT EXISTS(SELECT 1 FROM app_social_freshness WHERE install_id=? AND context_id=? AND receipt_id=?)",params![self.install(),context,receipt],|r|r.get(0))?)
    }
    pub fn app_social_freshness_receipts(&self, context: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        let rows=conn.prepare("SELECT receipt_id FROM app_social_freshness WHERE install_id=? AND context_id=? ORDER BY completed_at DESC,handle LIMIT 20")?
            .query_map(params![self.install(), context], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn app_social_sources_show(&self, context: &str) -> Result<Value> {
        let row = self.conn().query_row("SELECT revision,handles_json,updated FROM app_social_sources WHERE install_id=? AND context_id=?", params![self.install(), context], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?))).optional()?;
        let (revision, raw, updated) = row.unwrap_or((0, String::from("[]"), 0.0));
        let handles = serde_json::from_str::<Vec<String>>(&raw)
            .map_err(|_| Error::rejected("social source settings are corrupt"))?;
        let conn = self.conn();
        let freshness: Vec<Value> = conn.prepare("SELECT handle,receipt_id,completed_at FROM app_social_freshness WHERE install_id=? AND context_id=? ORDER BY completed_at DESC,handle")?
            .query_map(params![self.install(), context], |r| Ok(json!({"handle":r.get::<_,String>(0)?,"receipt_id":r.get::<_,String>(1)?,"completed_at":r.get::<_,f64>(2)?})))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(
            json!({"revision":revision,"handles":handles,"updated_at":updated,"freshness":freshness}),
        )
    }

    fn social_effect_in(conn: &impl StoreConn, install: &str, id: &str) -> Result<Value> {
        let row=conn.query_row("SELECT context_id,draft_id,revision,request_id,digest,frozen_json,state,approval_id,outcome_json,created,updated FROM app_social_effects WHERE install_id=? AND effect_id=?",params![install,id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,f64>(9)?,r.get::<_,f64>(10)?))).optional()?.ok_or_else(||Error::rejected("social draft effect not found"))?;
        let frozen: Value = serde_json::from_str(&row.5)
            .map_err(|_| Error::rejected("social draft effect is corrupt"))?;
        Ok(
            json!({"effect":{"effect_id":id,"install_id":install,"context_id":row.0,"draft_id":row.1,"revision":row.2,"request":row.3,"digest":row.4,"state":row.6,"needs_you":row.6=="waiting","approval_id":row.7,"authorization_kind":"social_draft","authority":frozen,"record":{},"outcome":row.8.and_then(|v|serde_json::from_str::<Value>(&v).ok()),"created_at":row.9,"updated_at":row.10}}),
        )
    }
    pub fn app_social_effect_show(&self, id: &str) -> Result<Value> {
        Self::social_effect_in(&*self.conn(), self.install(), id)
    }
    pub fn app_social_effect_list(&self, context: &str) -> Result<Value> {
        let conn = self.conn();
        let ids=conn.prepare("SELECT effect_id FROM app_social_effects WHERE install_id=? AND context_id=? AND draft_id NOT IN (SELECT draft_id FROM app_social_drafts WHERE install_id=? AND context_id=? AND discarded_at IS NOT NULL) ORDER BY created DESC,effect_id LIMIT 101")?.query_map(params![self.install(),context,self.install(),context],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        if ids.len() > 100 {
            return Err(Error::rejected(
                "social effect inventory exceeds its supported bound",
            ));
        }
        let effects = ids
            .iter()
            .map(|id| {
                Self::social_effect_in(&*conn, self.install(), id).map(|v| v["effect"].clone())
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"effects":effects}))
    }
    pub fn app_social_effect_stage(&self, context: &str, stage: &EffectStage<'_>) -> Result<Value> {
        let EffectStage {
            draft,
            revision,
            request,
            effect_id,
            frozen,
            approval,
        } = *stage;
        let digest = crate::store::app_runs::material_digest(frozen);
        let packed = serde_json::to_string(frozen).map_err(|e| Error::internal(e.to_string()))?;
        self.write_tx(|tx|{
            if let Some((existing,))=tx.query_row("SELECT effect_id FROM app_social_effects WHERE request_id=?",[request],|r|Ok((r.get::<_,String>(0)?,))).optional()? {
                let shown=Self::social_effect_in(tx,self.install(),&existing)?;
                if shown["effect"]["digest"]!=digest{return Err(Error::rejected("social effect request ID was reused for changed frozen material"));}
                return Ok(shown);
            }
            let at=now();tx.execute("INSERT INTO app_social_effects(effect_id,install_id,context_id,draft_id,revision,request_id,digest,frozen_json,state,approval_id,created,updated) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",params![effect_id,self.install(),context,draft,revision,request,digest,packed,"waiting",approval,at,at])?;
            Self::social_effect_in(tx,self.install(),effect_id)
        })
    }
    pub fn app_social_effect_decide(
        &self,
        id: &str,
        digest: &str,
        accept: bool,
    ) -> Result<Option<Value>> {
        self.write_tx(|tx|{
            let state:Option<(String,String)>=tx.query_row("SELECT state,digest FROM app_social_effects WHERE install_id=? AND effect_id=?",params![self.install(),id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((state,stored))=state else{return Ok(None)};
            if stored!=digest||state!="waiting"{return Err(Error::rejected("social effect digest is stale or effect is no longer waiting"));}
            tx.execute("UPDATE app_social_effects SET state=?,updated=? WHERE install_id=? AND effect_id=? AND state='waiting' AND digest=?",params![if accept{"approved"}else{"declined"},now(),self.install(),id,digest])?;
            Ok(Some(Self::social_effect_in(tx,self.install(),id)?))
        })
    }
    pub fn app_social_effect_media_key(&self, id: &str, key: Option<&str>) -> Result<()> {
        self.write_tx(|tx| {
            let old: Option<Option<String>> = tx.query_row("SELECT media_key FROM app_social_effects WHERE install_id=? AND effect_id=? AND state='approved'",params![self.install(),id],|r|r.get(0)).optional()?;
            let Some(old)=old else{return Err(Error::rejected("social effect is no longer approved"));};
            if old.as_deref().is_some_and(|saved|Some(saved)!=key){return Err(Error::rejected("social effect media import key changed"));}
            tx.execute("UPDATE app_social_effects SET media_key=?,updated=? WHERE install_id=? AND effect_id=? AND state='approved'",params![key,now(),self.install(),id])?;
            Ok(())
        })
    }
    pub fn app_social_effect_claim_send(&self, id: &str, digest: &str) -> Result<Option<Value>> {
        self.write_tx(|tx|{
            let changed=tx.execute("UPDATE app_social_effects SET state='sending',updated=? WHERE install_id=? AND effect_id=? AND digest=? AND state='approved'",params![now(),self.install(),id,digest])?;
            if changed!=1{return Ok(None)};Ok(Some(Self::social_effect_in(tx,self.install(),id)?))
        })
    }
    pub fn app_social_effect_finish(
        &self,
        id: &str,
        state: &str,
        outcome: &Value,
    ) -> Result<Value> {
        if !matches!(state, "posted" | "refused") {
            return Err(Error::rejected("social effect outcome state is invalid"));
        }
        self.write_tx(|tx|{
            let changed=tx.execute("UPDATE app_social_effects SET state=?,outcome_json=?,updated=? WHERE install_id=? AND effect_id=? AND state='sending'",params![state,serde_json::to_string(outcome).map_err(|e|Error::internal(e.to_string()))?,now(),self.install(),id])?;
            if changed!=1{return Err(Error::rejected("social effect is not in sending state"));}
            Self::social_effect_in(tx,self.install(),id)
        })
    }

    pub fn app_social_sources_save(
        &self,
        context: &str,
        expected: i64,
        handles: &[String],
        request: &str,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(request, "social source request ID")?;
        if expected < 0 || handles.len() > 20 {
            return Err(Error::rejected(
                "social source settings exceed their supported bound",
            ));
        }
        let mut normalized = Vec::with_capacity(handles.len());
        for value in handles {
            let raw = value.trim().strip_prefix('@').unwrap_or(value.trim());
            let h = raw.to_ascii_lowercase();
            if h.is_empty()
                || h.len() > 30
                || !h.as_bytes()[0].is_ascii_alphanumeric()
                || !h
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.'))
            {
                return Err(Error::rejected("social source handle is invalid"));
            }
            if normalized.contains(&h) {
                return Err(Error::rejected("social source handles must be unique"));
            }
            normalized.push(h);
        }
        let canonical =
            serde_json::to_string(&normalized).map_err(|e| Error::internal(e.to_string()))?;
        let intent = crate::store::app_runs::material_digest(
            &json!({"context":context,"expected_revision":expected,"handles":normalized}),
        );
        self.write_tx(|tx|{
            if let Some(saved)=tx.query_row("SELECT intent_digest FROM app_social_source_requests WHERE request_id=?",[request],|r|r.get::<_,String>(0)).optional()? {
                if saved!=intent { return Err(Error::rejected("social source request ID was reused for another update")); }
                let row=tx.query_row("SELECT revision,handles_json,updated FROM app_social_sources WHERE install_id=? AND context_id=?",params![self.install(),context],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,f64>(2)?)))?;
                return Ok(json!({"revision":row.0,"handles":serde_json::from_str::<Vec<String>>(&row.1).map_err(|_|Error::rejected("social source settings are corrupt"))?,"updated_at":row.2}));
            }
            let current=tx.query_row("SELECT revision FROM app_social_sources WHERE install_id=? AND context_id=?",params![self.install(),context],|r|r.get::<_,i64>(0)).optional()?.unwrap_or(0);
            if current!=expected { return Err(Error::conflict(current,"social source settings revision is stale")); }
            let next=current+1;let at=now();
            tx.execute("INSERT INTO app_social_sources(install_id,context_id,revision,handles_json,updated) VALUES(?,?,?,?,?) ON CONFLICT(context_id) DO UPDATE SET revision=excluded.revision,handles_json=excluded.handles_json,updated=excluded.updated",params![self.install(),context,next,canonical,at])?;
            tx.execute("INSERT INTO app_social_source_requests(request_id,install_id,context_id,intent_digest,at) VALUES(?,?,?,?,?)",params![request,self.install(),context,intent,at])?;
            Ok(json!({"revision":next,"handles":normalized,"updated_at":at}))
        })
    }
}
