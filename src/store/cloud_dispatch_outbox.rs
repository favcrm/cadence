//! CAD-720 source facts for a future hosted assignment custody adapter.
//!
//! This table is deliberately inert: there is no publisher or reader.
//! A dispatch row is not an assignment. In particular, `message_id` is
//! never a remote turn token, and rows without a durable turn claim must
//! not be exposed as actionable work.

use crate::error::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{now, Store};

impl Store {
    /// Called only inside the same transaction that commits a dispatch.
    /// A failed insert aborts the message/task transition with it.
    pub(super) fn record_cloud_dispatch_tx(
        tx: &Connection,
        message_id: &str,
        task: Option<(&str, i64)>,
    ) -> Result<()> {
        let (source, alias, body, message_task): (String, String, String, Option<String>) = tx
            .query_row(
                "SELECT source, alias, body, task_id FROM messages WHERE id=?1",
                [message_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        if !matches!(source.as_str(), "dispatch" | "job_dispatch")
            || message_task.as_deref() != task.map(|(id, _)| id)
        {
            return Err(Error::rejected(
                "cloud dispatch outbox requires the exact committed dispatch message",
            ));
        }
        let expected_head: Option<String> = if let Some((task_id, revision)) = task {
            let (current_message, current_revision, assignee, base_sha): (
                Option<String>,
                i64,
                Option<String>,
                Option<String>,
            ) = tx.query_row(
                "SELECT dispatch_message, revision, assignee, base_sha FROM tasks WHERE id=?1",
                [task_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            if current_message.as_deref() != Some(message_id)
                || current_revision != revision
                || assignee.as_deref() != Some(alias.as_str())
            {
                return Err(Error::rejected(
                    "cloud dispatch outbox task revision differs from the committed kickoff",
                ));
            }
            base_sha
        } else {
            None
        };
        let payload = json!({
            "message_id": message_id,
            "source": source,
            "task_id": task.map(|(id, _)| id),
            "task_revision": task.map(|(_, rev)| rev),
            "audience_agent": alias,
            "expected_head": expected_head,
            "body_sha256": format!("sha256:{:x}", Sha256::digest(body.as_bytes())),
        });
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(payload.to_string().as_bytes())
        );
        tx.execute(
            "INSERT INTO cloud_dispatch_outbox(
                message_id,source,task_id,task_revision,audience_agent,expected_head,
                payload_digest,created) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                message_id,
                source,
                task.map(|(id, _)| id),
                task.map(|(_, rev)| rev),
                alias,
                expected_head,
                digest,
                now()
            ],
        )?;
        Ok(())
    }

    /// Atomically bind a remote turn and issuer-derived audience to one
    /// still-current committed dispatch. This is not wired to any RPC or
    /// network path until AOS-77 defines and verifies the issuer mapping.
    #[allow(dead_code)]
    pub(crate) fn claim_cloud_dispatch_turn(
        &self,
        message_id: &str,
        organization_id: &str,
        audience_agent: &str,
    ) -> Result<String> {
        if organization_id.trim().is_empty() {
            return Err(Error::rejected("cloud organization is required"));
        }
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let row: Option<(
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
        )> = tx
            .query_row(
                "SELECT audience_agent,task_id,task_revision,organization_id,remote_turn_id
                 FROM cloud_dispatch_outbox WHERE message_id=?1",
                [message_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((alias, task_id, revision, claimed_org, claimed_turn)) = row else {
            return Err(Error::rejected("no committed cloud dispatch outbox record"));
        };
        if alias != audience_agent {
            return Err(Error::rejected(
                "cloud dispatch audience differs from committed target",
            ));
        }
        let (message_alias, state): (String, String) = tx.query_row(
            "SELECT alias,state FROM messages WHERE id=?1",
            [message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if message_alias != alias || state != "queued" {
            return Err(Error::rejected("cloud dispatch is no longer queued"));
        }
        if let Some(task_id) = task_id {
            let (current_message, current_revision, current_alias, task_state): (
                Option<String>,
                i64,
                Option<String>,
                String,
            ) = tx.query_row(
                "SELECT dispatch_message,revision,assignee,state FROM tasks WHERE id=?1",
                [&task_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            if current_message.as_deref() != Some(message_id)
                || Some(current_revision) != revision
                || current_alias.as_deref() != Some(alias.as_str())
                || task_state != "dispatched"
            {
                return Err(Error::rejected("cloud dispatch task is no longer current"));
            }
        }
        if let Some(turn) = claimed_turn {
            if claimed_org.as_deref() != Some(organization_id) {
                return Err(Error::rejected(
                    "cloud dispatch claim has a different organization",
                ));
            }
            return Ok(turn);
        }
        let turn = format!("remote-{}", Uuid::new_v4().simple());
        tx.execute(
            "UPDATE cloud_dispatch_outbox SET organization_id=?2,remote_turn_id=?3,claimed=?4
             WHERE message_id=?1 AND remote_turn_id IS NULL",
            params![message_id, organization_id, turn, now()],
        )?;
        tx.commit()?;
        Ok(turn)
    }
}
