//! Read projections for app runs and their frozen teams.

use super::{approval_human, RunTeam, APP_RUN_APPROVAL_STREAM, APP_RUN_LEGACY_RETIRED};
use crate::error::{Error, Result};
use crate::store::{Store, StoreConn};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

impl Store {
    /// CAD-622: each run of `install_id` as the snapshot's frozen
    /// team-role map (`team_roles`: role → alias, exactly the roles the
    /// installation team supplied when the run was created), its frozen
    /// `inputs` and the aliases it assigned to its steps — the run-time
    /// half of an app's team, read from the run's own snapshot rather
    /// than the current workflow or team record. A snapshot written
    /// before `team_roles` existed returns `roles: None`: its inputs
    /// merged the role bindings into content and nothing recorded which
    /// keys were roles, so the guard holds rather than inferring them
    /// from today's team or reading content values as agents. A snapshot
    /// that cannot be parsed refuses; it is never read as "nobody".
    pub fn app_run_team_inputs(&self, install_id: &str) -> Result<Vec<RunTeam>> {
        let texts = self
            .conn()
            .prepare("SELECT id,state,snapshot FROM app_runs WHERE install_id=? ORDER BY created")?
            .query_map([install_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // Runs whose terminal legacy ambiguity the operator has retired.
        let retired: BTreeSet<String> = self
            .conn()
            .prepare(
                "SELECT DISTINCT e.job_id FROM events e JOIN app_runs r ON r.id=e.job_id \
                 WHERE e.kind=?1 AND e.alias=?2 AND r.install_id=?3",
            )?
            .query_map(
                params![APP_RUN_LEGACY_RETIRED, APP_RUN_APPROVAL_STREAM, install_id],
                |r| r.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::new();
        for (id, state, text) in texts {
            let snapshot: Value = serde_json::from_str(&text)
                .map_err(|_| Error::internal("app run snapshot is unreadable"))?;
            let snapshot = snapshot
                .as_object()
                .ok_or_else(|| Error::internal("app run snapshot is unreadable"))?;
            let roles = match snapshot.get("team_roles") {
                None => None,
                Some(value) => Some(
                    serde_json::from_value(value.clone())
                        .map_err(|_| Error::internal("app run team roles are unreadable"))?,
                ),
            };
            let inputs = match snapshot.get("inputs") {
                None => BTreeMap::new(),
                Some(value) => serde_json::from_value(value.clone())
                    .map_err(|_| Error::internal("app run inputs are unreadable"))?,
            };
            let assigned = snapshot
                .get("assignments")
                .and_then(Value::as_object)
                .map(|m| {
                    m.values()
                        .filter_map(|step| step["alias"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            // A host-execution workflow names no owner and takes no
            // team, so it froze no role binding into its inputs.
            let may_have_roles = snapshot
                .get("workflow")
                .and_then(|workflow| workflow.get("execution"))
                .and_then(Value::as_str)
                != Some("host");
            out.push(RunTeam {
                legacy_retired: retired.contains(&id),
                id,
                state,
                roles,
                inputs,
                assigned,
                may_have_roles,
            });
        }
        Ok(out)
    }

    pub fn app_run_show(&self, id: &str) -> Result<Value> {
        self.read_tx(|conn| Self::app_run_show_in(&conn, id))
    }
    pub(in crate::store) fn app_run_show_in(conn: &impl StoreConn, id: &str) -> Result<Value> {
        let mut value=conn.query_row("SELECT install_id,epoch,snapshot,snapshot_digest,project_link,state,approved_digest FROM app_runs WHERE id=?",[id],|r|Ok(json!({"id":id,"install_id":r.get::<_,String>(0)?,"epoch":r.get::<_,i64>(1)?,"snapshot":r.get::<_,String>(2)?,"snapshot_digest":r.get::<_,String>(3)?,"project_link":r.get::<_,Option<String>>(4)?,"state":r.get::<_,String>(5)?,"approved_digest":r.get::<_,Option<String>>(6)?}))).optional()?.ok_or_else(||Error::rejected("unknown app run"))?;
        value["snapshot"] = serde_json::from_str(value["snapshot"].as_str().unwrap())
            .map_err(|e| Error::internal(e.to_string()))?;
        value["context_id"] = json!(conn.query_row(
            "SELECT context_id FROM app_runs WHERE id=?",
            [id],
            |r| r.get::<_, Option<String>>(0)
        )?);
        let (created, updated): (f64, f64) = conn.query_row(
            "SELECT created,updated FROM app_runs WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        value["created"] = json!(created as i64);
        value["updated"] = json!(updated as i64);
        // CAD-1123 R4: the latest approval of this run, when recorded.
        if let Some((payload, at)) = conn
            .query_row(
                "SELECT payload,at FROM events WHERE job_id=?1 AND alias=?2 AND kind='app_run_approved' ORDER BY seq DESC LIMIT 1",
                params![id, APP_RUN_APPROVAL_STREAM],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)),
            )
            .optional()?
        {
            // CAD-1374: an unreadable approval payload is a refusal, never a
            // silent legacy downgrade. Only a missing human is a legacy approval.
            let recorded: Value = serde_json::from_str(&payload)
                .map_err(|_| Error::internal("approval record is unreadable"))?;
            if !recorded.is_object() {
                return Err(Error::internal("approval record is unreadable"));
            }
            let by = recorded["by"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| "operator".into());
            let human = approval_human(Some(&recorded))?;
            value["approval"] = json!({"by": by, "at": at as i64, "human": human});
        }
        if let Some((kind, reason, step_id)) = conn
            .query_row(
                "SELECT kind,reason,step_id FROM app_run_failures WHERE run_id=?",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        {
            value["failure"] = json!({"kind":kind,"reason":reason,"step_id":step_id});
        }
        // CAD-1142: surface the step's failure reason. `app_step_failed_in`
        // records the cause on the bound task's `error`; project it onto the
        // step so the board and screen show *why* a step failed instead of a
        // bare `state: failed` (Demo s1 runs failed with no reason anywhere).
        value["steps"]=Value::Array(conn.query_vec("SELECT s.step_id,s.task_id,s.state,s.message_id,t.error FROM app_run_steps s LEFT JOIN tasks t ON t.id=s.task_id WHERE s.run_id=? ORDER BY s.step_id",[id],|r|{let mut step=json!({"step_id":r.get::<_,String>(0)?,"task_id":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"message_id":r.get::<_,Option<String>>(3)?});if let Some(reason)=r.get::<_,Option<String>>(4)?.filter(|reason|!reason.trim().is_empty()){step["reason"]=json!(reason);}Ok(step)})?);
        value["artifacts"]=Value::Array(conn.query_vec("SELECT id,step_id,digest,media_type,length(content) FROM app_run_artifacts WHERE run_id=? ORDER BY step_id",[id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"step_id":r.get::<_,String>(1)?,"digest":r.get::<_,String>(2)?,"media_type":r.get::<_,String>(3)?,"size":r.get::<_,i64>(4)?})))?);
        value["reviews"]=Value::Array(conn.query_vec("SELECT step_id,artifact_digest,reviewer,decision,rationale,asset_receipt_id,asset_digest FROM app_run_reviews WHERE run_id=?",[id],|r|{
            let mut review=json!({"step_id":r.get::<_,String>(0)?,"artifact_digest":r.get::<_,String>(1)?,"reviewer":r.get::<_,String>(2)?,"decision":r.get::<_,String>(3)?,"rationale":r.get::<_,String>(4)?});
            let asset:Option<String>=r.get(5)?;
            let digest:Option<String>=r.get(6)?;
            if let (Some(asset),Some(digest))=(asset,digest) {
                review["asset_receipt_id"]=json!(asset);
                review["asset_digest"]=json!(digest);
            }
            Ok(review)
        })?);
        value["disposition"] = Self::operator_disposition_json(conn, id)?;
        value["ticket"] = Self::operator_ticket_json(conn, id)?;
        Ok(value)
    }
    pub fn app_run_list(&self, install_id: Option<&str>) -> Result<Value> {
        self.app_run_list_filtered(install_id, None)
    }
    pub fn app_run_list_filtered(
        &self,
        install_id: Option<&str>,
        context_id: Option<&str>,
    ) -> Result<Value> {
        if let Some(context) = context_id {
            self.app_context_show(
                install_id
                    .ok_or_else(|| Error::rejected("context run filter requires installation"))?,
                context,
            )?;
        }
        let ids = self
            .conn()
            .prepare(
                "SELECT id FROM app_runs WHERE (?1 IS NULL OR install_id=?1) AND (?2 IS NULL OR context_id=?2) ORDER BY created",
            )?
            .query_map(params![install_id,context_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"runs":ids.iter().map(|id|self.app_run_show(id)).collect::<Result<Vec<_>>>()?}))
    }
}
