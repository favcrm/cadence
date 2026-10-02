//! Server-derived accepted-artifact authorization, distinct from worker grants.
use super::*;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::path::Path;
use super::StoreConn;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_effect_authorizations(
 effect_id TEXT PRIMARY KEY REFERENCES platform_effects(effect_id),
 install_id TEXT NOT NULL, context_id TEXT, run_id TEXT NOT NULL,
 artifact_id TEXT NOT NULL, authority TEXT NOT NULL, authority_digest TEXT NOT NULL,
 input_digest TEXT NOT NULL, release_digest TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS app_effect_install_scope
 ON app_effect_authorizations(install_id,context_id,effect_id);
";

#[derive(Debug, Clone)]
pub struct AppEffectPermit {
    pub effect_id: String,
    pub platform: String,
    pub account: String,
    pub tool: String,
    pub input: Value,
    pub input_digest: String,
    pub authority_digest: String,
    pub provenance: Value,
}

/// Provenance carries this core receipt. Excluding that field prevents a
/// self-referential hash; the release digest additionally pins all provenance.
pub fn authority_digest(authority: &Value) -> String {
    let mut core = authority.clone();
    if let Some(object) = core.as_object_mut() {
        object.remove("provenance");
    }
    app_runs::material_digest(&core)
}

pub fn input_digest(input: &Value) -> String {
    app_runs::artifact_digest(input.to_string().as_bytes())
}

fn release_digest(row: &EffectRow, authority: &Value) -> String {
    app_runs::material_digest(&json!({"kind":"app-release-v1","request":row.request,
        "platform":row.platform,"account":row.account,"tool":row.tool,"input":row.input,
        "preview":row.preview,"source_hash":row.source_hash,"scopes":row.scopes,
        "authority":authority}))
}

fn validate_child(row: &EffectRow, authority: &Value) -> Result<()> {
    if !authority.is_object()
        || authority["schema"] != 1
        || !authority["install_id"].is_string()
        || !authority["run_id"].is_string()
        || !authority["artifact_id"].is_string()
        || !authority["provenance"].is_object()
        || row.input["provenance"] != authority["provenance"]
        || row.input["provenance"]["effect_id"] != row.effect_id
        || row.input["provenance"]["authorization_kind"] != "app_artifact"
        || row.input["provenance"]["authority_digest"] != authority_digest(authority)
        || row.input["schema"] != 1
        || !row.input["title"].is_string()
        || row.source_name.is_some()
        || row.task.is_some()
        || row.input.get("project").is_some()
        || row.input.get("attachments").is_some()
        || row.input.get("asset") != authority.get("asset")
        || authority["provenance"].get("asset") != authority.get("asset")
    {
        return Err(Error::rejected(
            "app effect child does not match its server-owned input",
        ));
    }
    let body = row.input["body"]
        .as_str()
        .ok_or_else(|| Error::rejected("app effect body is not text"))?;
    let digest = app_runs::artifact_digest(body.as_bytes());
    if row.source_hash.as_deref() != Some(digest.as_str())
        || authority["artifact_digest"] != digest
        || authority["provenance"]["artifact_digest"] != digest
        || authority["provenance"]["install_id"] != authority["install_id"]
        || authority["provenance"]["run_id"] != authority["run_id"]
        || authority["provenance"]["artifact_id"] != authority["artifact_id"]
        || serde_json::to_vec(&row.input)?.len() > 64 * 1024
        || row.preview.len() > 16 * 1024
    {
        return Err(Error::rejected(
            "app effect artifact receipt or input/preview bound is invalid",
        ));
    }
    Ok(())
}

fn child_in(conn: &impl super::StoreConn, id: &str) -> Result<(EffectRow, Value, String)> {
    let row = conn
        .query_row(
            "SELECT * FROM platform_effects WHERE effect_id=?",
            [id],
            effects::EffectRow::from_row,
        )
        .optional()?
        .ok_or_else(|| Error::rejected("app effect does not exist"))?;
    let kind: String = conn.query_row(
        "SELECT authorization_kind FROM platform_effects WHERE effect_id=?",
        [id],
        |r| r.get(0),
    )?;
    if kind != "app_artifact" {
        return Err(Error::rejected(
            "effect is not authorized by an app artifact",
        ));
    }
    let child = conn.query_row("SELECT install_id,context_id,run_id,artifact_id,authority,authority_digest,input_digest,release_digest FROM app_effect_authorizations WHERE effect_id=?",[id],|r| Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?)))
        .optional()?.ok_or_else(|| Error::rejected("app effect authorization child is missing"))?;
    let authority: Value = serde_json::from_str(&child.4)?;
    validate_child(&row, &authority)?;
    if authority["install_id"] != child.0
        || authority["context"]["id"].as_str() != child.1.as_deref()
        || authority["run_id"] != child.2
        || authority["artifact_id"] != child.3
        || authority_digest(&authority) != child.5
        || input_digest(&row.input) != child.6
        || release_digest(&row, &authority) != child.7
    {
        return Err(Error::rejected(
            "app effect authorization receipt is corrupt",
        ));
    }
    Ok((row, authority, child.7))
}

fn envelope(row: &EffectRow, authority: &Value, digest: &str) -> Value {
    // App receipts preserve historical decisions and uncertain outcomes even
    // after reconciliation closes. They are not legacy pending-effect v1.
    let mut record = row.to_record();
    record["kind"] = json!("app_artifact_effect");
    json!({"effect":{"schema":1,"authorization_kind":"app_artifact",
        "effect_id":row.effect_id,"request":row.request,"state":row.state,
        "needs_you":row.needs_you,"digest":digest,"record":record,"authority":authority}})
}

/// The provider opens the existing database read-only. It cannot recover a
/// daemon or manufacture an authorization while preparing a filesystem write.
pub fn read_execution_permit(path: &Path, effect_id: &str) -> Result<AppEffectPermit> {
    let conn = open_read_only(path)?;
    let (row, authority, _) = child_in(&conn, effect_id)?;
    if row.state != "executing" {
        return Err(Error::rejected("app effect has no executing claim"));
    }
    Ok(AppEffectPermit {
        effect_id: row.effect_id,
        platform: row.platform,
        account: row.account,
        tool: row.tool,
        input_digest: input_digest(&row.input),
        input: row.input,
        authority_digest: authority_digest(&authority),
        provenance: authority["provenance"].clone(),
    })
}

impl Store {
    pub(super) fn app_effect_invalidate_in(
        conn: &impl super::StoreConn,
        install: &str,
        context: Option<&str>,
        binding: Option<&str>,
        bundle_digest: Option<&str>,
    ) -> Result<()> {
        let ids: Vec<String> = conn.query_vec(
            "SELECT e.effect_id FROM platform_effects e JOIN app_effect_authorizations a ON a.effect_id=e.effect_id WHERE a.install_id=? AND (? IS NULL OR a.context_id=?) AND (? IS NULL OR json_extract(a.authority,'$.binding.id')=?) AND (? IS NULL OR json_extract(a.authority,'$.bundle_digest')=?) AND e.authorization_kind='app_artifact' AND e.state IN ('waiting','decided')",
            params![
                install,
                context,
                context,
                binding,
                binding,
                bundle_digest,
                bundle_digest
            ],
            |r| r.get::<_, String>(0),
        )?;
        for id in ids {
            conn.execute("UPDATE platform_effects SET state='closed',close_reason='app_authority_changed',updated_at=? WHERE effect_id=? AND state IN ('waiting','decided')",params![now(),id])?;
            Self::event(
                conn,
                platform::PLATFORM_STREAM,
                EFFECT_CANCELLED_EVENT,
                json!({"effect_id":id,"install_id":install,"authorization_kind":"app_artifact","reason":"app_authority_changed"}),
            )?;
        }
        Ok(())
    }
    pub fn app_publication_material(
        &self,
        run: &str,
        artifact: &str,
        bundle: &str,
        slot: &str,
    ) -> Result<Value> {
        Self::app_publication_material_in(&self.conn(), run, artifact, bundle, slot)
    }

    /// Completion receipts describe the actual accepted historical turns.
    /// Retiring those endpoints does not alter their recorded acceptance.
    /// The installation, context and binding are nevertheless current here.
    pub(crate) fn app_publication_material_in(
        conn: &impl super::StoreConn,
        id: &str,
        artifact: &str,
        bundle: &str,
        slot: &str,
    ) -> Result<Value> {
        let run = Self::app_run_show_in(conn, id)?;
        if run["snapshot"]["bundle_digest"] != bundle {
            return Err(Error::rejected(
                "publication bundle differs from the frozen run",
            ));
        }
        Self::app_completed_current_in(conn, &run)?;
        if run["state"] != "succeeded"
            || run["approved_digest"] != run["snapshot_digest"]
            || app_runs::material_digest(&run["snapshot"]) != run["snapshot_digest"]
            || !matches!(run["snapshot"]["schema"].as_u64(), Some(3 | 4))
            || run["snapshot"]["publication"]["slot"] != slot
        {
            return Err(Error::rejected(
                "publication requires an approved completed run with its exact frozen slot",
            ));
        }
        let proof: super::app_bindings::BindingProof =
            serde_json::from_value(run["snapshot"]["publication"]["binding"].clone())
                .map_err(|_| Error::rejected("draft has no frozen publication binding"))?;
        if !super::app_bindings::binding_current_in(
            conn,
            run["install_id"].as_str().unwrap(),
            run["context_id"].as_str(),
            slot,
            &proof,
        )? {
            return Err(Error::rejected(
                "frozen publication binding is no longer current",
            ));
        }
        let (step,message,turn,producer,digest,media,body):(String,String,String,String,String,String,Vec<u8>) = conn.query_row(
            "SELECT step_id,message_id,turn_id,producer,digest,media_type,substr(content,1,?) FROM app_run_artifacts WHERE id=? AND run_id=?",
            params![(app_runs::ARTIFACT_BYTES+1) as i64,artifact,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?
            .ok_or_else(||Error::rejected("artifact does not belong to this run"))?;
        if body.is_empty()
            || body.len() > app_runs::ARTIFACT_BYTES
            || !matches!(media.as_str(), "text/plain" | "text/markdown")
            || app_runs::artifact_digest(&body) != digest
        {
            return Err(Error::rejected(
                "accepted artifact integrity or type is invalid",
            ));
        }
        let producer_receipt =
            historical_step_receipt(conn, &run, &step, &message, &producer, &turn)?;
        if producer_receipt["material"]["kind"] != "produce_text"
            || producer_receipt["material"]["outcome"] != "succeeded"
            || producer_receipt["material"]["artifacts"][0]["text"]
                .as_str()
                .map(|text| text.as_bytes())
                != Some(body.as_slice())
        {
            return Err(Error::rejected(
                "artifact differs from its actual completed producer receipt",
            ));
        }
        let review: (String,String,String,String,Option<String>,Option<String>) = conn.query_row(
            "SELECT step_id,reviewer,message_id,rationale,asset_receipt_id,asset_digest FROM app_run_reviews WHERE run_id=? AND artifact_id=? AND artifact_digest=? AND decision='approve' ORDER BY step_id LIMIT 1",
            params![id,artifact,digest],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?
            .ok_or_else(||Error::rejected("artifact has no accepted independent review"))?;
        if review.1 == producer {
            return Err(Error::rejected("artifact review is not independent"));
        }
        let review_turn: String = conn.query_row(
            "SELECT turn_id FROM messages WHERE id=?",
            [&review.2],
            |r| r.get(0),
        )?;
        let reviewed =
            historical_step_receipt(conn, &run, &review.0, &review.2, &review.1, &review_turn)?;
        let required_asset_slot = run["snapshot"]["workflow"]["required_asset_slot"].as_str();
        let asset = match (&review.4, &review.5) {
            (None, None) if required_asset_slot.is_none() => None,
            (Some(receipt_id), Some(asset_digest)) => {
                let (receipt, _) = super::app_capabilities::asset_material_in(conn, receipt_id)?;
                if receipt["receipt_schema"] != 2
                    || receipt["run_id"] != id
                    || receipt["step_id"] != step
                    || required_asset_slot.is_some_and(|slot| receipt["slot"] != slot)
                    || receipt["asset"]["digest"] != *asset_digest
                    || run["snapshot"]["capabilities"][receipt["slot"].as_str().unwrap_or("")]
                        ["digest"]
                        != receipt["binding_digest"]
                {
                    return Err(Error::rejected(
                        "reviewed binary asset no longer matches its run receipt",
                    ));
                }
                Some(
                    json!({"receipt_id":receipt_id,"receipt_digest":receipt["digest"],
                    "binding_digest":receipt["binding_digest"],
                    "digest":receipt["asset"]["digest"],"media_type":receipt["asset"]["media_type"],
                    "size":receipt["asset"]["size"]}),
                )
            }
            _ => {
                return Err(Error::rejected(
                    "required reviewed binary asset pin is absent or incomplete",
                ))
            }
        };
        if reviewed["material"]["kind"] != "review_text"
            || reviewed["material"]["decision"] != "approve"
            || reviewed["material"]["producer_step_id"] != step
            || reviewed["material"]["artifact_sha256"] != digest
            || reviewed["material"]["rationale"] != review.3
            || reviewed["material"]["asset_receipt_id"] != json!(review.4)
            || reviewed["material"]["asset_sha256"] != json!(review.5)
        {
            return Err(Error::rejected(
                "stored review differs from its actual accepted turn",
            ));
        }
        let text = String::from_utf8(body)
            .map_err(|_| Error::rejected("accepted artifact is not UTF-8 text"))?;
        let mut material = json!({"run":run,"binding":proof,"artifact":{"id":artifact,"digest":digest,"media_type":media,"text":text},
            "producer_receipt":producer_receipt,"review_receipt":reviewed});
        if let Some(asset) = asset {
            material["asset"] = asset;
        }
        Ok(material)
    }

    pub fn app_effect_show(&self, id: &str) -> Result<Value> {
        return self.write_tx(|conn| {

                    let (row, authority, digest) = child_in(&conn, id)?;
                    Ok(envelope(&row, &authority, &digest))
        });
        }

    /// A provider may have committed even when its confirmation failed.
    /// Persist that distinction, independently of read-back, without replay.
    pub fn app_effect_uncertain(&self, id: &str, outcome: &Value) -> Result<Value> {
        if outcome["kind"] != "uncertain"
            || outcome["error"].as_str().is_none_or(str::is_empty)
            || !matches!(outcome["verified"], Value::Bool(_)) && outcome["verified"] != "unknown"
        {
            return Err(Error::rejected("invalid uncertain app artifact outcome"));
        }
        return self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let (row, authority, digest) = child_in(&tx, id)?;
                    if row.state != "executing" {
                        return Err(Error::rejected("app effect is not executing"));
                    }
                    let changed = tx.execute(
                        "UPDATE platform_effects SET state='reconcile',outcome=?,needs_you=1,updated_at=? WHERE effect_id=? AND state='executing' AND authorization_kind='app_artifact'",
                        params![outcome.to_string(), now(), id],
                    )?;
                    if changed != 1 {
                        return Err(Error::rejected("app effect uncertainty claim changed"));
                    }
                    Self::event(
                        &tx,
                        platform::PLATFORM_STREAM,
                        EFFECT_NEEDS_YOU_EVENT,
                        json!({"effect_id":id,"authorization_kind":"app_artifact","reason":"app artifact completion is uncertain — reconcile","verified":outcome["verified"]}),
                    )?;
                    let (row, _, _) = child_in(&tx, id)?;
                    let result = envelope(&row, &authority, &digest);
                    Ok(result)
        });
        }

    /// Operator resolution changes bookkeeping only. The exact historical
    /// child and digest survive, including when its installation is gone.
    pub fn app_effect_resolve(&self, id: &str, digest: &str, resolution: &str) -> Result<Value> {
        return self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let (row, authority, stored_digest) = child_in(&tx, id)?;
                    if digest != stored_digest {
                        return Err(Error::rejected("app effect release digest changed"));
                    }
                    let changed = match resolution {
                        "close" if row.state == "reconcile" => tx.execute(
                            "UPDATE platform_effects SET state='closed',close_reason='operator_reconciled',needs_you=0,updated_at=? WHERE effect_id=? AND state='reconcile' AND authorization_kind='app_artifact'",
                            params![now(), id],
                        )?,
                        "acknowledge" if matches!(row.state.as_str(), "done" | "failed") && row.needs_you => tx.execute(
                            "UPDATE platform_effects SET needs_you=0,updated_at=? WHERE effect_id=? AND state IN ('done','failed') AND needs_you=1 AND authorization_kind='app_artifact'",
                            params![now(), id],
                        )?,
                        "close" | "acknowledge" => {
                            return Err(Error::rejected("app effect is not eligible for this resolution"));
                        }
                        _ => return Err(Error::rejected("app effect resolution must be close or acknowledge")),
                    };
                    if changed != 1 {
                        return Err(Error::rejected("app effect resolution state changed"));
                    }
                    Self::event(
                        &tx,
                        platform::PLATFORM_STREAM,
                        if resolution == "close" {
                            EFFECT_CANCELLED_EVENT
                        } else {
                            "effect_acknowledged"
                        },
                        json!({"effect_id":id,"authorization_kind":"app_artifact","resolution":resolution,"digest":digest}),
                    )?;
                    let (resolved, _, _) = child_in(&tx, id)?;
                    let result = envelope(&resolved, &authority, &stored_digest);
                    Ok(result)
        });
        }
    pub fn app_effect_list(&self, install: Option<&str>, context: Option<&str>) -> Result<Value> {
        if context.is_some() && install.is_none() {
            return Err(Error::rejected("context filter requires installation"));
        }
        return self.write_tx(|conn| {

                    let mut stmt = conn.prepare("SELECT effect_id FROM app_effect_authorizations WHERE (? IS NULL OR install_id=?) AND (? IS NULL OR context_id=?) ORDER BY effect_id LIMIT 100")?;
                    let ids = stmt
                        .query_map(params![install, install, context, context], |r| {
                            r.get::<_, String>(0)
                        })?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    let mut list = Vec::new();
                    for id in ids {
                        let (row, authority, digest) = child_in(&conn, &id)?;
                        list.push(envelope(&row, &authority, &digest)["effect"].clone());
                    }
                    Ok(json!({"effects":list}))
        });
        }
    pub fn app_effect_is_child(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT authorization_kind='app_artifact' FROM platform_effects WHERE effect_id=?",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    /// Atomic row/child/audit insertion; retries require the entire frozen
    /// authority and presentation, not merely matching provider arguments.
    pub fn app_effect_stage(&self, row: &EffectRow, authority: &Value) -> Result<Value> {
        validate_child(row, authority)?;
        return self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let existing = tx
                        .query_opt(
                            "SELECT effect_id FROM platform_effects WHERE request=?",
                            [&row.request],
                            |r| r.get::<_, String>(0),
                        )?;
                    if let Some(id) = existing {
                        let (existing, stored, digest) = child_in(&tx, &id)?;
                        if release_digest(row, authority) != digest || stored != *authority {
                            return Err(Error::rejected(
                                "app release request already names different approved material",
                            ));
                        }
                        return Ok(envelope(&existing, &stored, &digest));
                    }
                    let digest = release_digest(row, authority);
                    tx.execute("INSERT INTO platform_effects(effect_id,request,agent,platform,account,tool,label,input,input_summary,preview,source_name,source_hash,scopes,task,state,staged_at,updated_at,authorization_kind) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,'waiting',?,?,'app_artifact')",
                        params![row.effect_id,row.request,row.agent,row.platform,row.account,row.tool,row.label,row.input.to_string(),row.input_summary,row.preview,row.source_name,row.source_hash,serde_json::to_string(&row.scopes)?,row.task,now(),now()])?;
                    tx.execute(
                        "INSERT INTO app_effect_authorizations VALUES(?,?,?,?,?,?,?,?,?)",
                        params![
                            row.effect_id,
                            authority["install_id"].as_str(),
                            authority["context"]["id"].as_str(),
                            authority["run_id"].as_str(),
                            authority["artifact_id"].as_str(),
                            authority.to_string(),
                            authority_digest(authority),
                            input_digest(&row.input),
                            digest
                        ],
                    )?;
                    Self::event(
                        &tx,
                        platform::PLATFORM_STREAM,
                        EFFECT_REQUESTED_EVENT,
                        json!({"effect_id":row.effect_id,"request":row.request,"authorization_kind":"app_artifact","install_id":authority["install_id"],"digest":digest}),
                    )?;
                    let (stored, authority, digest) = child_in(&tx, &row.effect_id)?;
                    Ok(envelope(&stored, &authority, &digest))
        });
        }

    /// Eligibility and the exactly-one claim share a transaction. A caller
    /// must hold the daemon release lock until provider commit/readback;
    /// this method drops SQLite before any adapter is called.
    pub(crate) fn app_effect_claim<F>(
        &self,
        id: &str,
        digest: &str,
        eligible: F,
    ) -> Result<Option<EffectRow>>
    where
        F: FnOnce(&super::WriteTxn<'_>, &Value) -> Result<bool>,
    {
        return self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let (row, authority, stored_digest) = child_in(&tx, id)?;
                    if digest != stored_digest {
                        return Err(Error::rejected("app effect release digest changed"));
                    }
                    if row.state != "decided" || !eligible(&*tx, &authority)? {
                        return Ok(None);
                    }
                    let count = tx.execute("UPDATE platform_effects SET state='executing',updated_at=? WHERE effect_id=? AND state='decided' AND authorization_kind='app_artifact'",params![now(),id])?;
                    if count != 1 {
                        return Ok(None);
                    }
                    let mut claimed = row;
                    claimed.state = "executing".into();
                    Ok(Some(claimed))
        });
        }
}

fn historical_step_receipt(
    conn: &impl super::StoreConn,
    run: &Value,
    step: &str,
    message: &str,
    actor: &str,
    turn: &str,
) -> Result<Value> {
    let (spec,identity,state,message_id,result_digest):(String,String,String,Option<String>,Option<String>) = conn.query_row(
        "SELECT spec,identity_digest,state,message_id,result_digest FROM app_run_steps WHERE run_id=? AND step_id=?",
        params![run["id"].as_str(),step],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    let spec: app_runs::LocalStep = serde_json::from_str(&spec)?;
    let assignment = &run["snapshot"]["assignments"][step];
    if state != "succeeded"
        || message_id.as_deref() != Some(message)
        || spec.assignee != actor
        || assignment["alias"] != actor
        || assignment["identity_digest"] != identity
        || app_runs::material_digest(&assignment["identity"]) != identity
        || !run["snapshot"]["workflow"]["steps"]
            .as_array()
            .is_some_and(|steps| {
                steps
                    .iter()
                    .any(|frozen| serde_json::to_value(&spec).is_ok_and(|actual| &actual == frozen))
            })
    {
        return Err(Error::rejected(
            "completed step assignment receipt is invalid",
        ));
    }
    let (alias, state, stored_turn, result): (String, String, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT alias,state,turn_id,result FROM messages WHERE id=?",
            [message],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    if alias != actor
        || state != "completed"
        || stored_turn.as_deref() != Some(turn)
        || turn.is_empty()
    {
        return Err(Error::rejected(
            "completed material turn receipt is invalid",
        ));
    }
    let result: Value = serde_json::from_str(
        result
            .as_deref()
            .ok_or_else(|| Error::rejected("completed material result is missing"))?,
    )?;
    let material: Value = match result["text"].as_str() {
        Some(text) => serde_json::from_str(text)?,
        None => result,
    };
    let digest =
        app_runs::material_digest(&json!({"material":material,"producer":actor,"message":message}));
    if result_digest.as_deref() != Some(digest.as_str())
        || material["run_id"] != run["id"]
        || material["step_id"] != step
        || material["revision"] != 1
        || material["schema"] != 1
        || material["kind"] != spec.kind
    {
        return Err(Error::rejected(
            "completed material result digest is invalid",
        ));
    }
    Ok(
        json!({"step_id":step,"message_id":message,"turn_id":turn,"actor":actor,"identity_digest":identity,"result_digest":digest,"material":material}),
    )
}
