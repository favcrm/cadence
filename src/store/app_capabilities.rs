//! Durable, run-scoped results from reviewed read/draft capabilities.
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
 UNIQUE(run_id,step_id,slot,request_id));
CREATE INDEX IF NOT EXISTS app_capability_results_run
 ON app_capability_results(run_id,created,id);
";

pub const RESULT_BYTES: usize = 256 * 1024;
pub const ASSET_BYTES: usize = 2 * 1024 * 1024;

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

impl Store {
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
        if run["install_id"] != install
            || run["context_id"] != json!(context)
            || run["state"] != "succeeded"
        {
            return Err(Error::rejected(
                "source receipt is outside this installation/context or incomplete",
            ));
        }
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

    pub(crate) fn app_capability_result_for_request(
        &self,
        run: &str,
        step: &str,
        slot: &str,
        request: &str,
    ) -> Result<Option<Value>> {
        let id = self.conn().query_row(
            "SELECT id FROM app_capability_results WHERE run_id=? AND step_id=? AND slot=? AND request_id=?",
            params![run,step,slot,request], |r| r.get::<_,String>(0),
        ).optional()?;
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
            "input_digest":input_digest,"result":result,"asset_digest":asset_digest
        }));
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
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
        let existing = tx.query_row(
            "SELECT id,input_digest FROM app_capability_results WHERE run_id=? AND step_id=? AND slot=? AND request_id=?",
            params![run,step,slot,request],
            |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)),
        ).optional()?;
        if let Some((existing_id, existing_input)) = existing {
            if existing_input != input_digest {
                return Err(Error::rejected(
                    "capability request id is already used for different input",
                ));
            }
            tx.commit()?;
            drop(conn);
            return self.app_capability_result(&existing_id);
        }
        tx.execute(
            "INSERT INTO app_capability_results VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
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
        tx.commit()?;
        drop(conn);
        self.app_capability_result(id)
    }

    pub(crate) fn app_capability_result(&self, id: &str) -> Result<Value> {
        let conn = self.conn();
        let row = conn.query_row(
            "SELECT run_id,step_id,message_id,turn_id,slot,request_id,binding_digest,input_digest,result,result_digest,asset_type,asset_digest,length(asset) FROM app_capability_results WHERE id=?",
            [id], |r| Ok((
                r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,
                r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,
                r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,
                r.get::<_,String>(9)?,r.get::<_,Option<String>>(10)?,
                r.get::<_,Option<String>>(11)?,r.get::<_,Option<i64>>(12)?
            )),
        ).optional()?.ok_or_else(|| Error::rejected("app capability result unavailable"))?;
        let result: Value = serde_json::from_str(&row.8)?;
        let receipt = json!({"id":id,"run_id":row.0,"step_id":row.1,"message_id":row.2,
            "slot":row.4,"request_id":row.5,"binding_digest":row.6,
            "input_digest":row.7,"result":result,"digest":row.9,
            "asset":row.10.as_ref().map(|kind|json!({"media_type":kind,"digest":row.11,"size":row.12}))});
        if app_runs::material_digest(&json!({
            "run_id":receipt["run_id"],"step_id":receipt["step_id"],
            "message_id":receipt["message_id"],"turn_id":row.3,
            "slot":receipt["slot"],"request_id":receipt["request_id"],
            "binding_digest":receipt["binding_digest"],"input_digest":receipt["input_digest"],
            "result":receipt["result"],"asset_digest":receipt["asset"]["digest"]
        })) != receipt["digest"]
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
        let conn = self.conn();
        let (kind,digest,bytes):(String,String,Vec<u8>) = conn.query_row(
            "SELECT asset_type,asset_digest,asset FROM app_capability_results WHERE id=? AND asset IS NOT NULL",
            [id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional()?.ok_or_else(||Error::rejected("app capability asset unavailable"))?;
        if bytes.is_empty()
            || bytes.len() > ASSET_BYTES
            || app_runs::artifact_digest(&bytes) != digest
        {
            return Err(Error::rejected("app capability asset receipt is corrupt"));
        }
        use base64::Engine;
        Ok(json!({"receipt_id":id,"media_type":kind,"digest":digest,
            "size":bytes.len(),"base64":base64::engine::general_purpose::STANDARD.encode(bytes)}))
    }
}
