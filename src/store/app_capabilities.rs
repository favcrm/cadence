//! Durable, run-scoped results from reviewed read/draft capabilities.
use super::StoreConn;
use super::*;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_capability_results(
 id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES app_runs(id),
 step_id TEXT NOT NULL, message_id TEXT NOT NULL, turn_id TEXT NOT NULL,
 slot TEXT NOT NULL, request_id TEXT NOT NULL, binding_digest TEXT NOT NULL,
 input_digest TEXT NOT NULL, result TEXT NOT NULL, result_digest TEXT NOT NULL,
 asset_type TEXT, asset_digest TEXT, asset BLOB, created REAL NOT NULL,
 UNIQUE(run_id,slot));
CREATE INDEX IF NOT EXISTS app_capability_results_run
 ON app_capability_results(run_id,created,id);
CREATE TABLE IF NOT EXISTS app_capability_claims(
 run_id TEXT NOT NULL REFERENCES app_runs(id), slot TEXT NOT NULL,
 step_id TEXT NOT NULL, request_id TEXT NOT NULL, binding_digest TEXT NOT NULL,
 input_digest TEXT NOT NULL, call_id TEXT NOT NULL UNIQUE, created REAL NOT NULL,
 PRIMARY KEY(run_id,slot));
";

pub const RESULT_BYTES: usize = 256 * 1024;
pub const ASSET_BYTES: usize = 2 * 1024 * 1024;

/// Return the exact immutable asset and its complete provider receipt. This
/// works inside a finishing/release SQL transaction, so neither review nor
/// release can observe a different row between the proof and byte read.
pub(crate) fn asset_material_in(
    conn: &impl super::StoreConn,
    id: &str,
) -> Result<(Value, Vec<u8>)> {
    let row = conn
        .query_row(
            "SELECT run_id,step_id,message_id,turn_id,slot,request_id,binding_digest,input_digest,
                result,result_digest,asset_type,asset_digest,
                substr(asset,1,?2),length(asset),receipt_schema
         FROM app_capability_results WHERE id=?1 AND asset IS NOT NULL",
            params![id, (ASSET_BYTES + 1) as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, String>(10)?,
                    r.get::<_, String>(11)?,
                    r.get::<_, Vec<u8>>(12)?,
                    r.get::<_, i64>(13)?,
                    r.get::<_, i64>(14)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::rejected("app capability asset unavailable"))?;
    let result: Value = serde_json::from_str(&row.8)?;
    let asset = json!({"media_type":row.10,"digest":row.11,"size":row.12.len()});
    let hash_material = if row.14 == 2 {
        json!({"run_id":row.0,"step_id":row.1,"message_id":row.2,"turn_id":row.3,
            "slot":row.4,"request_id":row.5,"binding_digest":row.6,
            "input_digest":row.7,"result":result,"asset":asset})
    } else {
        json!({"run_id":row.0,"step_id":row.1,"message_id":row.2,"turn_id":row.3,
            "slot":row.4,"request_id":row.5,"binding_digest":row.6,
            "input_digest":row.7,"result":result,"asset_digest":asset["digest"]})
    };
    if !matches!(row.14, 1 | 2)
        || row.12.is_empty()
        || row.13 <= 0
        || row.13 as usize != row.12.len()
        || row.12.len() > ASSET_BYTES
        || app_runs::artifact_digest(&row.12) != asset["digest"]
        || !asset["media_type"].as_str().is_some_and(|mime| {
            mime.len() <= 127
                && mime.split_once('/').is_some_and(|(major, minor)| {
                    !major.is_empty()
                        && !minor.is_empty()
                        && major.bytes().chain(minor.bytes()).all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || matches!(byte, b'.' | b'+' | b'-')
                        })
                })
        })
        || app_runs::material_digest(&hash_material) != row.9
    {
        return Err(Error::rejected("app capability asset receipt is corrupt"));
    }
    Ok((
        json!({"id":id,"run_id":row.0,"step_id":row.1,"slot":row.4,
        "receipt_schema":row.14,
        "binding_digest":row.6,"digest":row.9,"asset":asset}),
        row.12,
    ))
}

/// Local's executing adapter independently checks the persisted bytes after
/// the broker's checked claim and before writing the confined outbox item.
pub fn read_asset_material(path: &std::path::Path, id: &str) -> Result<(Value, Vec<u8>)> {
    asset_material_in(&open_read_only(path)?, id)
}

pub(crate) struct AppCapabilityRecord<'a> {
    pub id: &'a str,
    pub run: &'a str,
    pub step: &'a str,
    pub message: &'a str,
    pub turn: &'a str,
    pub slot: &'a str,
    pub request: &'a str,
    pub binding_digest: &'a str,
    pub input_digest: &'a str,
    pub result: &'a Value,
    pub asset: Option<(&'a str, &'a [u8])>,
}

pub(crate) struct AppCapabilityClaim<'a> {
    pub run: &'a str,
    pub step: &'a str,
    pub message: &'a str,
    pub turn: &'a str,
    pub slot: &'a str,
    pub request: &'a str,
    pub binding_digest: &'a str,
    pub input_digest: &'a str,
    pub call_id: &'a str,
}

impl Store {
    /// Reserve the one paid operation for this run slot before provider I/O.
    /// A failed/uncertain result can only retry the same operation and key.
    pub(crate) fn app_capability_claim(&self, claim: AppCapabilityClaim<'_>) -> Result<()> {
        let AppCapabilityClaim {
            run,
            step,
            message,
            turn,
            slot,
            request,
            binding_digest,
            input_digest,
            call_id,
        } = claim;
        return self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let active: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM app_run_steps s JOIN app_runs r ON r.id=s.run_id
                         JOIN messages m ON m.id=s.message_id WHERE s.run_id=? AND s.step_id=?
                         AND s.message_id=? AND s.state='dispatched' AND r.state='running'
                         AND r.approved_digest=r.snapshot_digest AND m.state='running' AND m.turn_id=?)",
                        params![run, step, message, turn],
                        |r| r.get(0),
                    )?;
                    if !active {
                        return Err(Error::rejected(
                            "app capability needs its active assigned turn",
                        ));
                    }
                    tx.execute(
                        "INSERT OR IGNORE INTO app_capability_claims VALUES(?,?,?,?,?,?,?,?)",
                        params![
                            run,
                            slot,
                            step,
                            request,
                            binding_digest,
                            input_digest,
                            call_id,
                            now()
                        ],
                    )?;
                    let existing: (String, String, String, String, String) = tx.query_row(
                        "SELECT step_id,request_id,binding_digest,input_digest,call_id
                         FROM app_capability_claims WHERE run_id=? AND slot=?",
                        params![run, slot],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )?;
                    if existing
                        != (
                            step.into(),
                            request.into(),
                            binding_digest.into(),
                            input_digest.into(),
                            call_id.into(),
                        )
                    {
                        return Err(Error::rejected(
                            "approved run capability slot already claimed another operation",
                        ));
                    }
                    Ok(())
        });
    }

    pub(crate) fn app_selected_source_input(
        &self,
        install: &str,
        context: Option<&str>,
        receipt_id: &str,
        post_id: &str,
    ) -> Result<String> {
        let receipt = self.app_capability_result(receipt_id)?;
        let run = self.app_run_show(
            receipt["run_id"]
                .as_str()
                .ok_or_else(|| Error::rejected("source run receipt is invalid"))?,
        )?;
        if run["install_id"] != install || run["context_id"] != json!(context) {
            return Err(Error::rejected(
                "source receipt is outside this installation/context or incomplete",
            ));
        }
        source_receipt_recoverable_in(&self.conn(), &run, &receipt)?;
        let posts = receipt["result"]["posts"]
            .as_array()
            .ok_or_else(|| Error::rejected("source receipt has no posts"))?;
        let mut matches = posts
            .iter()
            .filter(|post| post["id"].as_str() == Some(post_id));
        let post = matches
            .next()
            .ok_or_else(|| Error::rejected("selected post is absent from source receipt"))?;
        if matches.next().is_some() {
            return Err(Error::rejected(
                "selected post id is ambiguous in source receipt",
            ));
        }
        let caption = post["caption"]
            .as_str()
            .ok_or_else(|| Error::rejected("selected post has no normalized caption"))?;
        crate::issue::workflow::source_input_line(caption)
    }

    pub(crate) fn app_capability_receipt_for_turn(
        &self,
        id: &str,
        alias: &str,
        message: &str,
        token: &str,
        bundle: &str,
    ) -> Result<Value> {
        let receipt = self.app_capability_result(id)?;
        let (run, step, _) = self.app_capability_turn(
            alias,
            message,
            token,
            receipt["slot"]
                .as_str()
                .ok_or_else(|| Error::rejected("receipt slot is invalid"))?,
            bundle,
        )?;
        if run["id"] != receipt["run_id"] {
            return Err(Error::rejected("capability receipt belongs to another run"));
        }
        let current = run["snapshot"]["workflow"]["steps"]
            .as_array()
            .and_then(|steps| steps.iter().find(|candidate| candidate["id"] == step))
            .ok_or_else(|| Error::rejected("current step is not in frozen workflow"))?;
        if receipt["step_id"] != step
            && !current["dependencies"].as_array().is_some_and(|deps| {
                deps.iter()
                    .any(|dependency| dependency == &receipt["step_id"])
            })
        {
            return Err(Error::rejected(
                "capability receipt is not a dependency of this turn",
            ));
        }
        Ok(receipt)
    }

    pub(crate) fn app_capability_result_for_slot(
        &self,
        run: &str,
        slot: &str,
    ) -> Result<Option<Value>> {
        let id = self
            .conn()
            .query_row(
                "SELECT id FROM app_capability_results WHERE run_id=? AND slot=?",
                params![run, slot],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        id.map(|id| self.app_capability_result(&id)).transpose()
    }

    pub(crate) fn app_capability_record(&self, record: AppCapabilityRecord<'_>) -> Result<Value> {
        let AppCapabilityRecord {
            id,
            run,
            step,
            message,
            turn,
            slot,
            request,
            binding_digest,
            input_digest,
            result,
            asset,
        } = record;
        let serialized = serde_json::to_vec(result)?;
        if serialized.len() > RESULT_BYTES {
            return Err(Error::rejected("app capability result exceeds 256 KiB"));
        }
        if asset.is_some_and(|(_, bytes)| bytes.is_empty() || bytes.len() > ASSET_BYTES) {
            return Err(Error::rejected("app capability asset exceeds 2 MiB"));
        }
        let asset_digest = asset.map(|(_, bytes)| app_runs::artifact_digest(bytes));
        let digest = app_runs::material_digest(&json!({
            "run_id":run,"step_id":step,"message_id":message,"turn_id":turn,
            "slot":slot,"request_id":request,"binding_digest":binding_digest,
            "input_digest":input_digest,"result":result,
            "asset":asset.map(|(kind,bytes)|json!({"media_type":kind,
                "digest":app_runs::artifact_digest(bytes),"size":bytes.len()}))
        }));
        return self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let active: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM app_run_steps s JOIN app_runs r ON r.id=s.run_id
                         JOIN messages m ON m.id=s.message_id WHERE s.run_id=? AND s.step_id=?
                         AND s.message_id=? AND s.state='dispatched' AND r.state='running'
                         AND r.approved_digest=r.snapshot_digest AND m.state='running' AND m.turn_id=?)",
                        params![run, step, message, turn],
                        |r| r.get(0),
                    )?;
                    if !active {
                        return Err(Error::rejected(
                            "app capability turn ended before result was recorded",
                        ));
                    }
                    let claimed: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM app_capability_claims WHERE run_id=? AND slot=?
                         AND step_id=? AND request_id=? AND binding_digest=? AND input_digest=? AND call_id=?)",
                        params![run, slot, step, request, binding_digest, input_digest, id],
                        |r| r.get(0),
                    )?;
                    if !claimed {
                        return Err(Error::rejected(
                            "app capability result has no matching pre-call claim",
                        ));
                    }
                    let existing = tx.query_opt(
                        "SELECT id,step_id,request_id,binding_digest,input_digest FROM app_capability_results WHERE run_id=? AND slot=?",
                        params![run,slot],
                        |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)),
                    )?;
                    if let Some((
                        existing_id,
                        existing_step,
                        existing_request,
                        existing_binding,
                        existing_input,
                    )) = existing
                    {
                        if existing_step != step
                            || existing_request != request
                            || existing_binding != binding_digest
                            || existing_input != input_digest
                        {
                            return Err(Error::rejected(
                                "approved run capability slot already has its one result",
                            ));
                        }
                        return Self::app_capability_result_in(&tx, &existing_id);
                    }
                    tx.execute(
                        "INSERT INTO app_capability_results(id,run_id,step_id,message_id,turn_id,slot,
                            request_id,binding_digest,input_digest,result,result_digest,asset_type,
                            asset_digest,asset,created,receipt_schema) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,2)",
                        params![
                            id,
                            run,
                            step,
                            message,
                            turn,
                            slot,
                            request,
                            binding_digest,
                            input_digest,
                            String::from_utf8(serialized).map_err(|_| Error::internal("result encoding"))?,
                            digest,
                            asset.map(|(kind, _)| kind),
                            asset_digest,
                            asset.map(|(_, bytes)| bytes),
                            now()
                        ],
                    )?;
                    Self::event(
                        &tx,
                        Self::DAEMON_STREAM,
                        "app_capability_result_recorded",
                        json!({"run_id":run,"step_id":step,"slot":slot,"receipt_id":id,"digest":digest}),
                    )?;
                    Self::app_capability_result_in(&tx, id)
        });
    }

    pub(crate) fn app_capability_result(&self, id: &str) -> Result<Value> {
        Self::app_capability_result_in(&self.conn(), id)
    }

    fn app_capability_result_in(conn: &impl super::StoreConn, id: &str) -> Result<Value> {
        let row = conn.query_row(
            "SELECT run_id,step_id,message_id,turn_id,slot,request_id,binding_digest,input_digest,result,result_digest,asset_type,asset_digest,length(asset),receipt_schema FROM app_capability_results WHERE id=?",
            [id], |r| Ok((
                r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,
                r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,
                r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,
                r.get::<_,String>(9)?,r.get::<_,Option<String>>(10)?,
                r.get::<_,Option<String>>(11)?,r.get::<_,Option<i64>>(12)?,r.get::<_,i64>(13)?
            )),
        ).optional()?.ok_or_else(|| Error::rejected("app capability result unavailable"))?;
        let result: Value = serde_json::from_str(&row.8)?;
        let receipt = json!({"id":id,"run_id":row.0,"step_id":row.1,"message_id":row.2,
            "slot":row.4,"request_id":row.5,"binding_digest":row.6,
            "input_digest":row.7,"result":result,"digest":row.9,
            "asset":row.10.as_ref().map(|kind|json!({"media_type":kind,"digest":row.11,"size":row.12}))});
        let hash_material = if row.13 == 2 {
            json!({
                "run_id":receipt["run_id"],"step_id":receipt["step_id"],
                "message_id":receipt["message_id"],"turn_id":row.3,
                "slot":receipt["slot"],"request_id":receipt["request_id"],
                "binding_digest":receipt["binding_digest"],"input_digest":receipt["input_digest"],
                "result":receipt["result"],"asset":receipt["asset"]
            })
        } else {
            json!({
                "run_id":receipt["run_id"],"step_id":receipt["step_id"],
                "message_id":receipt["message_id"],"turn_id":row.3,
                "slot":receipt["slot"],"request_id":receipt["request_id"],
                "binding_digest":receipt["binding_digest"],"input_digest":receipt["input_digest"],
                "result":receipt["result"],"asset_digest":receipt["asset"]["digest"]
            })
        };
        if !matches!(row.13, 1 | 2)
            || app_runs::material_digest(&hash_material) != receipt["digest"]
        {
            return Err(Error::rejected("app capability result receipt is corrupt"));
        }
        Ok(receipt)
    }

    pub(crate) fn app_capability_results(&self, run: &str) -> Result<Value> {
        let conn = self.conn();
        let ids = conn.prepare("SELECT id FROM app_capability_results WHERE run_id=? ORDER BY created,id LIMIT 100")?
            .query_map([run], |r| r.get::<_,String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(conn);
        Ok(
            json!({"results":ids.iter().map(|id| self.app_capability_result(id))
            .collect::<Result<Vec<_>>>()?}),
        )
    }

    pub(crate) fn app_capability_asset(&self, id: &str) -> Result<Value> {
        let (receipt, bytes) = asset_material_in(&self.conn(), id)?;
        use base64::Engine;
        Ok(
            json!({"receipt_id":id,"media_type":receipt["asset"]["media_type"],"digest":receipt["asset"]["digest"],
            "size":bytes.len(),"base64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
        )
    }
}

/// A later failed step does not erase a broker-recorded read. Recovery is
/// limited to terminal runs with the original approval and the exact claim,
/// assigned message and turn that recorded this immutable receipt. The
/// caller must still verify the current installation/context and binding in
/// the run-creation transaction before freezing a selected post.
pub(super) fn source_receipt_recoverable_in(
    conn: &impl super::StoreConn,
    run: &Value,
    receipt: &Value,
) -> Result<()> {
    if !matches!(run["state"].as_str(), Some("succeeded" | "failed"))
        || run["approved_digest"] != run["snapshot_digest"]
        || receipt["run_id"] != run["id"]
    {
        return Err(Error::rejected(
            "source run has no recoverable approved receipt",
        ));
    }
    let belongs_to_broker_turn: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_capability_results r
         JOIN app_capability_claims c ON c.run_id=r.run_id AND c.slot=r.slot
         JOIN app_run_steps s ON s.run_id=r.run_id AND s.step_id=r.step_id
         JOIN messages m ON m.id=s.message_id
         WHERE r.id=?1 AND r.run_id=?2 AND r.message_id=s.message_id
         AND r.turn_id=m.turn_id AND c.step_id=r.step_id
         AND c.request_id=r.request_id AND c.binding_digest=r.binding_digest
         AND c.input_digest=r.input_digest AND c.call_id=r.id)",
        params![receipt["id"].as_str(), run["id"].as_str()],
        |r| r.get(0),
    )?;
    if !belongs_to_broker_turn {
        return Err(Error::rejected(
            "source receipt lacks its original broker turn",
        ));
    }
    Ok(())
}
