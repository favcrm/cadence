//! Host-step receipt settlement and terminal failure recording.

use super::material_digest;
use crate::error::{Error, Result};
use crate::store::{now, Store, StoreConn};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

impl Store {
    /// Called only while the daemon holds the installation's PM lock.
    ///
    /// CAD-1171: complete the host step only after every declared capability
    /// slot has a retained receipt. The operator's click ran the provider;
    /// there is no worker, message or artifact.
    pub fn app_run_host_step_succeeded(&self, run_id: &str, step_id: &str) -> Result<Value> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let run = Self::app_run_show_in(tx, run_id)?;
            if run["state"] != "running"
                || run["approved_digest"] != run["snapshot_digest"]
                || run["snapshot"]["workflow"]["execution"] != "host"
            {
                return Err(Error::rejected("host run is not active and approved"));
            }
            let slots = run["snapshot"]["workflow"]["capability_slots"]
                .as_array()
                .ok_or_else(|| Error::rejected("host run has no required capability slots"))?;
            if slots.is_empty() {
                return Err(Error::rejected("host run has no required capability slots"));
            }
            let (task_id, step_state): (String, String) = tx
                .query_row(
                    "SELECT task_id,state FROM app_run_steps WHERE run_id=? AND step_id=?",
                    params![run_id, step_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|_| Error::rejected("host step is absent"))?;
            if step_state != "dispatched" {
                return Err(Error::rejected("host step is not dispatched"));
            }
            let mut receipt_digests = Vec::with_capacity(slots.len());
            for slot in slots {
                let slot = slot
                    .as_str()
                    .ok_or_else(|| Error::rejected("host capability slot is invalid"))?;
                let digest: Option<String> = tx.query_row(
                    "SELECT result_digest FROM app_capability_results
                     WHERE run_id=? AND step_id=? AND slot=?",
                    params![run_id, step_id, slot],
                    |r| r.get(0),
                ).optional()?;
                let digest = digest.ok_or_else(|| {
                    Error::rejected("host step is missing a required capability receipt")
                })?;
                receipt_digests.push(json!({"slot":slot,"digest":digest}));
            }
            let result_digest = material_digest(&json!(receipt_digests));
            let now = now();
            tx.execute(
                "UPDATE app_run_steps SET state='succeeded',result_digest=? WHERE run_id=? AND step_id=? AND state='dispatched'",
                params![result_digest, run_id, step_id],
            )?;
            tx.execute(
                "UPDATE tasks SET state='done',error=NULL,updated=? WHERE id=?",
                params![now, task_id],
            )?;
            let pending: i64 = tx.query_row(
                "SELECT COUNT(*) FROM app_run_steps WHERE run_id=? AND state!='succeeded'",
                [run_id],
                |r| r.get(0),
            )?;
            if pending == 0 {
                tx.execute(
                    "UPDATE app_runs SET state='succeeded',updated=? WHERE id=? AND state='running'",
                    params![now, run_id],
                )?;
                tx.execute(
                    "UPDATE jobs SET state='done',updated=? WHERE id=?",
                    params![now, run_id],
                )?;
            }
            Self::event(
                tx,
                Self::DAEMON_STREAM,
                if pending == 0 {
                    "app_run_completed"
                } else {
                    "app_run_step_result_recorded"
                },
                json!({"run_id":run_id,"step_id":step_id,"host":true,"slots":slots.len()}),
            )?;
            Self::app_run_show_in(tx, run_id)
        })
    }

    /// A host capability refusal or uncertain result is terminal. Retain the
    /// claim and any earlier slot receipts, and never make it retryable via the
    /// background advance tick.
    pub fn app_run_host_step_failed(
        &self,
        run_id: &str,
        step_id: &str,
        kind: &str,
        reason: &str,
    ) -> Result<Value> {
        if !matches!(kind, "refused" | "uncertain") || reason.is_empty() || reason.len() > 1024 {
            return Err(Error::rejected("host failure record is invalid"));
        }
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let run = Self::app_run_show_in(tx, run_id)?;
            if run["state"] != "running"
                || run["approved_digest"] != run["snapshot_digest"]
                || run["snapshot"]["workflow"]["execution"] != "host"
            {
                return Err(Error::rejected("host run is not active and approved"));
            }
            let task_id: String = tx
                .query_row(
                    "SELECT task_id FROM app_run_steps WHERE run_id=? AND step_id=? AND state='dispatched'",
                    params![run_id, step_id],
                    |r| r.get(0),
                )
                .map_err(|_| Error::rejected("host step is not dispatched"))?;
            record_host_step_failure(tx, run_id, step_id, &task_id, kind, reason)?;
            Self::event(
                tx,
                Self::DAEMON_STREAM,
                "app_run_host_step_failed",
                json!({"run_id":run_id,"step_id":step_id,"kind":kind}),
            )?;
            Self::app_run_show_in(tx, run_id)
        })
    }

    /// CAD-1778: the fallback for `app_run_host_step_failed` when the run's
    /// persisted snapshot cannot be read. It checks only raw columns (the run
    /// is running and approved, the step is dispatched) and never parses the
    /// snapshot, so an unreadable run still ends with a durable failure.
    pub fn app_run_host_step_failed_unreadable(
        &self,
        run_id: &str,
        step_id: &str,
        kind: &str,
        reason: &str,
    ) -> Result<()> {
        if !matches!(kind, "refused" | "uncertain") || reason.is_empty() || reason.len() > 1024 {
            return Err(Error::rejected("host failure record is invalid"));
        }
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let task_id: String = tx
                .query_row(
                    "SELECT s.task_id FROM app_run_steps s JOIN app_runs r ON r.id=s.run_id WHERE s.run_id=? AND s.step_id=? AND s.state='dispatched' AND r.state='running' AND r.approved_digest IS NOT NULL",
                    params![run_id, step_id],
                    |r| r.get(0),
                )
                .map_err(|_| Error::rejected("host step is not dispatched"))?;
            record_host_step_failure(tx, run_id, step_id, &task_id, kind, reason)
        })
    }
}

/// Persist a terminal failure after the caller has checked the run and step
/// in the same transaction. Events and projections stay with the caller.
fn record_host_step_failure(
    tx: &impl StoreConn,
    run_id: &str,
    step_id: &str,
    task_id: &str,
    kind: &str,
    reason: &str,
) -> Result<()> {
    let now = now();
    tx.execute(
        "INSERT INTO app_run_failures(run_id,step_id,kind,reason,created) VALUES(?,?,?,?,?)",
        params![run_id, step_id, kind, reason, now],
    )?;
    tx.execute(
        "UPDATE app_run_steps SET state='failed' WHERE run_id=? AND step_id=? AND state='dispatched'",
        params![run_id, step_id],
    )?;
    tx.execute(
        "UPDATE tasks SET state='failed',error=?,updated=? WHERE id=?",
        params![reason, now, task_id],
    )?;
    tx.execute(
        "UPDATE app_runs SET state='failed',approved_digest=NULL,updated=? WHERE id=? AND state='running'",
        params![now, run_id],
    )?;
    tx.execute(
        "UPDATE jobs SET state='failed',updated=? WHERE id=?",
        params![now, run_id],
    )?;
    Ok(())
}
