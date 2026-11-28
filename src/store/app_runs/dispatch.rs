//! Dispatch approved app-run steps and recheck worker admission at claim time.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::{dispatch_receipt, kickoff_body, material_digest};
use crate::app_run_workflow::LocalStep;
use crate::error::{Error, Result};
use crate::store::{now, Message, Priority, Sender, Store, StoreConn, ENQUEUE_BYTES};

impl Store {
    pub fn app_run_dispatch(&self, id: &str, current_bundle: &str) -> Result<Value> {
        self.app_run_dispatch_as(id, current_bundle, None, None)
    }
    /// CAD-1374: `approved_by` is the human the caller fenced the approval
    /// under (`None` for a local or legacy approval). It must still be the
    /// latest approval inside this transaction, so a swapped approval cannot
    /// reuse a proof checked for the earlier one.
    pub fn app_run_dispatch_as(
        &self,
        id: &str,
        current_bundle: &str,
        approved_by: Option<&Value>,
        acting: Option<&Value>,
    ) -> Result<Value> {
        let operation = uuid::Uuid::new_v4().to_string();
        self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let run = Self::app_run_show_in(&tx, id)?;
                    Self::app_current_in(&tx, &run, current_bundle)?;
                    if run["approval"]["human"] != approved_by.cloned().unwrap_or(Value::Null) {
                        return Err(Error::gate_coded("approval_changed", "the run approval changed since its owner was checked; dispatch is refused"));
                    }
                    if !matches!(run["state"].as_str(), Some("approved" | "running"))
                        || run["approved_digest"] != run["snapshot_digest"]
                    {
                        return Err(Error::rejected("run execution approval is absent or stale"));
                    }
                    for assignment in run["snapshot"]["assignments"].as_object().unwrap().values() {
                        let alias = assignment["alias"].as_str().unwrap();
                        let worker = self.agent_in(&tx, alias)?;
                        if !(worker.enabled || Self::agent_auto_parked_in(&tx, &worker)?)
 || Self::app_binding_identity(&worker) != assignment["identity"] {
                            return Err(Error::rejected("registered app assignment changed"));
                        }
                    }
                    let rows=tx.query_vec("SELECT step_id,task_id,spec,identity_digest FROM app_run_steps WHERE run_id=? AND state='pending' ORDER BY step_id",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?;
                    for (step_id, task_id, spec, generation) in rows {
                        let step: LocalStep =
                            serde_json::from_str(&spec).map_err(|e| Error::internal(e.to_string()))?;
                        let mut dependencies = Vec::new();
                        let mut ready = true;
                        for dep in &step.dependencies {
                            let state: String = tx.query_row(
                                "SELECT state FROM app_run_steps WHERE run_id=? AND step_id=?",
                                params![id, dep],
                                |r| r.get(0),
                            )?;
                            if state != "succeeded" {
                                ready = false;
                                break;
                            }
                            if let Some(artifact)=tx.query_opt("SELECT id,digest,media_type,length(content) FROM app_run_artifacts WHERE run_id=? AND step_id=?",params![id,dep],|r|Ok(json!({"artifact_id":r.get::<_,String>(0)?,"producer_step_id":dep,"revision":1,"sha256":r.get::<_,String>(1)?,"media_type":r.get::<_,String>(2)?,"size":r.get::<_,i64>(3)?})))? {dependencies.push(artifact);}
                        }
                        if !ready {
                            continue;
                        }
                        // CAD-1171: a host capability step is dispatched
                        // without a message — the daemon runs it
                        // in-process for the operator's own click.
                        if step.kind == "capability" {
                            Self::event(
                                &tx,
                                Self::DAEMON_STREAM,
                                "app_run_step_dispatched",
                                dispatch_receipt(json!({"run_id":id,"step_id":step_id,"host":true,"snapshot_digest":run["snapshot_digest"]}), approved_by, acting, &operation, &format!("app-run:{id}/step:{step_id}/outcome"), "prepared"),
                            )?;
                            tx.execute("UPDATE tasks SET state='dispatched',revision=1,updated=? WHERE id=? AND state='draft'",params![now(),task_id])?;
                            tx.execute("UPDATE app_run_steps SET state='dispatched' WHERE run_id=? AND step_id=? AND state='pending'",params![id,step_id])?;
                            continue;
                        }
                        let worker = self.agent_in(&tx, &step.assignee)?;
                        let expected = &run["snapshot"]["assignments"][&step_id];
                        if material_digest(&Self::app_binding_identity(&worker)) != generation
                            || worker.role != expected["role"].as_str().unwrap()
                            || worker.provider != expected["provider"].as_str().unwrap()
                            || worker.endpoint_kind != expected["endpoint_kind"].as_str().unwrap()
                            || worker
                                .params
                                .as_ref()
                                .and_then(|p| p.get("upstream"))
                                .and_then(Value::as_str)
                                != run["snapshot"]["owner_pm"].as_str()
                        {
                            return Err(Error::rejected(
                                "registered assignment identity or group changed; create a new approved run",
                            ));
                        }
                        let message = format!("app-{id}-{step_id}-r1");
                        let body = kickoff_body(id, &step, &dependencies, &run["snapshot"])?;
                        if body.len() > ENQUEUE_BYTES {
                            return Err(Error::rejected(
                                "encoded app kickoff exceeds transport byte limit",
                            ));
                        }
                        // CAD-1374: the dispatch receipt is written in this
                        // transaction BEFORE the enqueue, so a failed enqueue rolls
                        // it back too. The sealed approver is the queue initiator;
                        // the sender stays unattributed and the executing worker
                        // is the assignee, unchanged.
                        Self::event(
                            &tx,
                            Self::DAEMON_STREAM,
                            "app_run_step_dispatched",
                            dispatch_receipt(json!({"run_id":id,"step_id":step_id,"message_id":message,"snapshot_digest":run["snapshot_digest"],"assignee":step.assignee}), approved_by, acting, &operation, &format!("app-run:{id}/step:{step_id}/outcome"), "dispatched"),
                        )?;
                        self.enqueue_tx_human(
                            &tx,
                            &step.assignee,
                            &body,
                            None,
                            &message,
                            "app_run_dispatch",
                            Some(&task_id),
                            None,
                            None,
                            &Sender::Unattributed,
                            Priority::Normal,
                            None,
                            None,
                            None,
                            approved_by,
                        )?;
                        tx.execute("UPDATE tasks SET state='dispatched',revision=1,dispatch_message=?,updated=? WHERE id=? AND state='draft'",params![message,now(),task_id])?;
                        tx.execute("UPDATE app_run_steps SET state='dispatched',message_id=? WHERE run_id=? AND step_id=? AND state='pending'",params![message,id,step_id])?;
                    }
                    tx.execute(
                        "UPDATE app_runs SET state='running',updated=? WHERE id=?",
                        params![now(), id],
                    )?;
                    Self::app_run_show_in(&tx, id)
        })
    }

    pub(in crate::store) fn app_message_admit_in(
        &self,
        conn: &impl StoreConn,
        message: &Message,
        bundle: &str,
    ) -> Result<()> {
        let (run_id,spec,generation):(String,String,String)=conn.query_row("SELECT run_id,spec,identity_digest FROM app_run_steps WHERE message_id=? AND state='dispatched'",[&message.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or_else(||Error::rejected("app message is not a current dispatched step"))?;
        let run = Self::app_run_show_in(conn, &run_id)?;
        Self::app_current_in(conn, &run, bundle)?;
        if run["state"] != "running" || run["approved_digest"] != run["snapshot_digest"] {
            return Err(Error::rejected("app message lacks current run approval"));
        }
        let step: LocalStep =
            serde_json::from_str(&spec).map_err(|e| Error::internal(e.to_string()))?;
        let actual = self.agent_in(conn, &step.assignee)?;
        let pinned = &run["snapshot"]["assignments"][&step.id];
        if !actual.enabled
            || actual.alias != message.alias
            || material_digest(&Self::app_binding_identity(&actual)) != generation
            || actual.role != pinned["role"].as_str().unwrap()
            || actual.provider != pinned["provider"].as_str().unwrap()
            || actual.endpoint_kind != pinned["endpoint_kind"].as_str().unwrap()
            || actual
                .params
                .as_ref()
                .and_then(|p| p.get("upstream"))
                .and_then(Value::as_str)
                != run["snapshot"]["owner_pm"].as_str()
        {
            return Err(Error::rejected(
                "app message assigned endpoint identity changed",
            ));
        }
        Ok(())
    }
    pub fn app_message_admit(&self, message: &Message, bundle: &str) -> Result<()> {
        self.read_tx(|conn| self.app_message_admit_in(&conn, message, bundle))
    }
}
