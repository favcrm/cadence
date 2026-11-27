//! Worker result decoding, active-turn proof, and settlement in the delivery transaction.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    app_worker_turn_reason, artifact_digest, local_token_current, material_digest, ARTIFACT_BYTES,
    RUN_ARTIFACT_BYTES,
};
use crate::app_run_workflow::LocalStep;
use crate::error::{Error, Result};
use crate::store::{app_capabilities, now, Message, Store, StoreConn};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::store) struct TextArtifact {
    media_type: String,
    text: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(in crate::store) enum LocalResult {
    #[serde(rename = "produce_text")]
    Produce {
        schema: u32,
        run_id: String,
        step_id: String,
        revision: u32,
        outcome: String,
        artifacts: Vec<TextArtifact>,
    },
    #[serde(rename = "review_text")]
    Review {
        schema: u32,
        run_id: String,
        step_id: String,
        revision: u32,
        producer_step_id: String,
        producer_revision: u32,
        artifact_sha256: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset_receipt_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset_sha256: Option<String>,
        decision: String,
        rationale: String,
    },
}

/// Bound applied before either parse path below. A max-size artifact can
/// expand when JSON-escaped, so the raw final text may legitimately exceed
/// `ARTIFACT_BYTES` by several times; anything beyond this still fails closed.
pub(in crate::store) const MAX_RESULT_TEXT_BYTES: usize = ARTIFACT_BYTES * 6 + 64 * 1024;

/// Locate exactly one top-level balanced-brace object in `text`, honouring
/// JSON string escapes. Returns `None` for zero, multiple, or unbalanced
/// brace spans, or when a fence marker is present anywhere outside the span.
fn extract_single_json_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut depth = 0usize;
    let mut start: Option<usize> = None;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    spans.push((start.take().expect("brace span start"), i + 1));
                }
            }
            _ => {}
        }
    }
    if in_string || depth != 0 || spans.len() != 1 {
        return None;
    }
    let (start, end) = spans[0];
    if text[..start].contains("```") || text[end..].contains("```") {
        return None;
    }
    Some(&text[start..end])
}

/// Pi sometimes surrounds its final material envelope with explanatory
/// prose, either bare or inside one standalone `json` fence. Accept exactly
/// one complete, bounded envelope in either form amid brace-free prose;
/// never search prose for the first of several parseable objects. Multiple,
/// conflicting, malformed, oversized, or extra-fence candidates fail closed.
/// The decoded result still passes the active-turn, pinned-run, artifact and
/// review checks below; unknown fields and forged identity are refused by
/// the strict `LocalResult` deserialization.
pub(in crate::store) fn parse_local_result_text(text: &str) -> Option<LocalResult> {
    if text.len() > MAX_RESULT_TEXT_BYTES {
        return None;
    }
    if text.contains("```") {
        let mut opening = None;
        let mut closing = None;
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            let marker = line.trim_end_matches(['\r', '\n']);
            match marker {
                "```json" if opening.is_none() && closing.is_none() => {
                    opening = Some((offset, offset + line.len()));
                }
                "```" if opening.is_some() && closing.is_none() => {
                    closing = Some((offset, offset + line.len()));
                }
                _ => {}
            }
            offset += line.len();
        }
        let ((open_start, body_start), (body_end, close_end)) = (opening?, closing?);
        let before = &text[..open_start];
        let after = &text[close_end..];
        if [before, after]
            .iter()
            .any(|part| part.contains("```") || part.contains('{') || part.contains('}'))
        {
            return None;
        }
        return serde_json::from_str::<LocalResult>(&text[body_start..body_end]).ok();
    }
    if let Ok(result) = serde_json::from_str::<LocalResult>(text) {
        return Some(result);
    }
    serde_json::from_str::<LocalResult>(extract_single_json_object(text)?).ok()
}

pub(in crate::store) enum AppCompletionProof {
    ActiveEndpoint { turn_id: String },
    Unproven,
    OperatorReconcile,
}
impl Store {
    pub(in crate::store) fn app_completion_proof(
        &self,
        conn: &impl StoreConn,
        message: &Message,
        result: &Value,
    ) -> Result<AppCompletionProof> {
        let current = self.message_in(conn, &message.id)?;
        if let Some(current) = current.filter(|m| {
            m.source == "app_run_dispatch" && m.state == "running" && m.alias == message.alias
        }) {
            if let Some(turn) = current
                .turn_id
                .filter(|turn| result.get("turn_id").and_then(Value::as_str) == Some(turn.as_str()))
            {
                return Ok(AppCompletionProof::ActiveEndpoint { turn_id: turn });
            }
        }
        Ok(AppCompletionProof::Unproven)
    }

    /// Material transitions occur only on the actual active app kickoff. This
    /// transaction records durable eligibility; it never acquires the PM lock.
    pub(in crate::store) fn app_run_finished_in(
        &self,
        tx: &impl StoreConn,
        message: &Message,
        status: &str,
        result: &Value,
        proof: &AppCompletionProof,
    ) -> Result<bool> {
        let Some(task_id) = message.task_id.as_deref() else {
            return Ok(false);
        };
        let Some((run_id,step_id,spec,generation,state,message_id))=tx.query_row("SELECT run_id,step_id,spec,identity_digest,state,message_id FROM app_run_steps WHERE task_id=?",[task_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?))).optional()? else{return Ok(false)};
        if matches!(proof, AppCompletionProof::OperatorReconcile) {
            return self
                .app_step_failed_in(
                    tx,
                    &run_id,
                    &step_id,
                    task_id,
                    "operator reconciliation preserves transport history, not material success",
                )
                .map(|_| true);
        }
        // CAD-1778: a worker call that failed after its claim already marked
        // its own step failed (`app_run_call_failed`); the message that owns
        // it may still close.
        if state == "failed"
            && message_id.as_deref() == Some(&message.id)
            && message.source == "app_run_dispatch"
        {
            return Ok(true);
        }
        // Reconciliation is transport history only, never material authority.
        if state != "dispatched"
            || message_id.as_deref() != Some(&message.id)
            || message.source != "app_run_dispatch"
        {
            return Err(Error::rejected("stale app completion association"));
        }
        let current = self
            .message_in(tx, &message.id)?
            .ok_or_else(|| Error::rejected("app kickoff disappeared"))?;
        let step: LocalStep =
            serde_json::from_str(&spec).map_err(|e| Error::internal(e.to_string()))?;
        let worker = self.agent_in(tx, &message.alias)?;
        let task = self.task_in(tx, task_id)?;
        if status != "completed" {
            // CAD-1142: the step's reason names what actually ended the
            // worker's turn (provider error, interruption, uncertain
            // outcome), not a bare "failed" — as a bounded
            // classification, never the provider's own words. The
            // message's `error`/`result.error` carries provider prose
            // verbatim and may hold credentials; it stays on the
            // operator-only message row while the step records only the
            // class (see `app_reason_class`).
            let detail = message
                .error
                .as_deref()
                .filter(|e| !e.trim().is_empty())
                .is_some()
                || result
                    .get("error")
                    .and_then(Value::as_str)
                    .is_some_and(|e| !e.trim().is_empty());
            let reason = app_worker_turn_reason(status, detail);
            return self
                .app_step_failed_in(tx, &run_id, &step_id, task_id, &reason)
                .map(|_| true);
        }
        let AppCompletionProof::ActiveEndpoint { turn_id } = proof else {
            return self
                .app_step_failed_in(
                    tx,
                    &run_id,
                    &step_id,
                    task_id,
                    "material completion lacks a proven active endpoint turn",
                )
                .map(|_| true);
        };
        let token = current
            .turn_id
            .as_deref()
            .ok_or_else(|| Error::rejected("app material result needs its active turn"))?;
        if turn_id != token
            || result.get("turn_id").and_then(Value::as_str) != Some(token)
            || material_digest(&Self::app_binding_identity(&worker)) != generation
            || message.alias != step.assignee
            || task.assignee.as_deref() != Some(&step.assignee)
            || task.revision != 1
            || task.dispatch_message.as_deref() != Some(&message.id)
            || !local_token_current(
                &worker.provider,
                &worker.endpoint_kind,
                worker.generation.as_deref(),
                token,
            )
        {
            return Err(Error::rejected(
                "app material result assignment or turn is stale",
            ));
        }
        // finish_in has already changed the message terminal state, but its
        // durable turn and association are still authoritative in this tx.
        let run = Self::app_run_show_in(tx, &run_id)?;
        let expected = &run["snapshot"]["assignments"][&step_id];
        let identity_matches = worker.role == expected["role"].as_str().unwrap()
            && worker.provider == expected["provider"].as_str().unwrap()
            && worker.endpoint_kind == expected["endpoint_kind"].as_str().unwrap()
            && worker
                .params
                .as_ref()
                .and_then(|p| p.get("upstream"))
                .and_then(Value::as_str)
                == run["snapshot"]["owner_pm"].as_str();
        if status != "completed"
            || run["state"] != "running"
            || run["approved_digest"] != run["snapshot_digest"]
            || !identity_matches
            || Self::app_current_in(tx, &run, run["snapshot"]["bundle_digest"].as_str().unwrap())
                .is_err()
        {
            return self
                .app_step_failed_in(
                    tx,
                    &run_id,
                    &step_id,
                    task_id,
                    "provider did not return a successful current run result",
                )
                .map(|_| true);
        };
        let decoded = if let Some(text) = result.get("text").and_then(Value::as_str) {
            parse_local_result_text(text)
        } else {
            serde_json::from_value::<LocalResult>(result.clone()).ok()
        };
        let Some(decoded) = decoded else {
            return self
                .app_step_failed_in(
                    tx,
                    &run_id,
                    &step_id,
                    task_id,
                    "invalid local result envelope",
                )
                .map(|_| true);
        };
        let normalized_result =
            serde_json::to_value(&decoded).map_err(|e| Error::internal(e.to_string()))?;
        let mut valid = false;
        match decoded {
            LocalResult::Produce {
                schema,
                run_id: r,
                step_id: s,
                revision,
                outcome,
                artifacts,
            } => {
                if schema == 1
                    && r == run_id
                    && s == step_id
                    && revision == 1
                    && step.kind == "produce_text"
                    && outcome == "succeeded"
                    && artifacts.len() == 1
                {
                    let artifact = &artifacts[0];
                    let total:i64=tx.query_row("SELECT COALESCE(SUM(length(content)),0) FROM app_run_artifacts WHERE run_id=?",[&run_id],|r|r.get(0))?;
                    if matches!(artifact.media_type.as_str(), "text/plain" | "text/markdown")
                        && !artifact.text.is_empty()
                        && artifact.text.len() <= ARTIFACT_BYTES
                        && total as usize + artifact.text.len() <= RUN_ARTIFACT_BYTES
                    {
                        let id = format!("artifact-{run_id}-{step_id}");
                        let digest = artifact_digest(artifact.text.as_bytes());
                        tx.execute(
                            "INSERT INTO app_run_artifacts VALUES(?,?,?,?,?,?,?,?,?,?)",
                            params![
                                id,
                                run_id,
                                step_id,
                                message.id,
                                token,
                                message.alias,
                                digest,
                                artifact.media_type,
                                artifact.text.as_bytes(),
                                now()
                            ],
                        )?;
                        Self::event(
                            tx,
                            Self::DAEMON_STREAM,
                            "app_run_artifact_recorded",
                            json!({"run_id":run_id,"step_id":step_id,"artifact_id":id,"sha256":digest,"producer":message.alias}),
                        )?;
                        valid = true;
                    }
                }
            }
            LocalResult::Review {
                schema,
                run_id: r,
                step_id: s,
                revision,
                producer_step_id,
                producer_revision,
                artifact_sha256,
                asset_receipt_id,
                asset_sha256,
                decision,
                rationale,
            } => {
                if schema == 1
                    && r == run_id
                    && s == step_id
                    && revision == 1
                    && producer_revision == 1
                    && step.kind == "review_text"
                    && step.dependencies == vec![producer_step_id.clone()]
                    && !rationale.trim().is_empty()
                    && rationale.len() <= 16 * 1024
                    && matches!(decision.as_str(), "approve" | "revise")
                {
                    if let Some((id,digest,producer))=tx.query_row("SELECT id,digest,producer FROM app_run_artifacts WHERE run_id=? AND step_id=?",params![run_id,producer_step_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()? {
                        let required_asset_slot = run["snapshot"]["workflow"]["required_asset_slot"].as_str();
                        let asset_valid = match (&asset_receipt_id,&asset_sha256) {
                            (None,None) => decision=="revise" || required_asset_slot.is_none(),
                            (Some(receipt_id),Some(asset_digest)) if decision=="approve" => {
                                app_capabilities::asset_material_in(tx,receipt_id)
                                    .is_ok_and(|(receipt,_)| receipt["receipt_schema"]==2
                                        && receipt["run_id"]==run_id
                                        && receipt["step_id"]==producer_step_id
                                        && required_asset_slot.is_none_or(|slot| receipt["slot"]==slot)
                                        && receipt["asset"]["digest"]==*asset_digest
                                        && run["snapshot"]["capabilities"][receipt["slot"].as_str().unwrap_or("")]["digest"]==receipt["binding_digest"])
                            },
                            _ => false,
                        };
                        // Approve names this artifact, this reviewer message,
                        // and the hash of the turn token completing now.
                        // Revise stays a recorded decision without that read.
                        let fetched = decision != "approve" || tx.query_row(
                            "SELECT 1 FROM app_run_artifact_fetches WHERE artifact_id=?1 AND message_id=?2 AND turn_hash=?3",
                            params![id, message.id, artifact_digest(token.as_bytes())],
                            |_| Ok(()),
                        ).optional()?.is_some();
                        if digest==artifact_sha256 && producer!=message.alias && asset_valid && fetched {
                            tx.execute("INSERT INTO app_run_reviews(run_id,step_id,artifact_id,artifact_digest,reviewer,message_id,decision,rationale,asset_receipt_id,asset_digest) VALUES(?,?,?,?,?,?,?,?,?,?)",params![run_id,step_id,id,digest,message.alias,message.id,decision,rationale,asset_receipt_id,asset_sha256])?;
                            Self::event(tx,Self::DAEMON_STREAM,"app_run_step_reviewed",json!({"run_id":run_id,"step_id":step_id,"artifact_digest":digest,"reviewer":message.alias,"decision":decision}))?;
                            valid=decision=="approve";
                        }
                    }
                }
            }
        }
        if !valid {
            return self
                .app_step_failed_in(
                    tx,
                    &run_id,
                    &step_id,
                    task_id,
                    "material output or pinned review refused",
                )
                .map(|_| true);
        };
        let result_digest = material_digest(
            &json!({"material":normalized_result,"producer":message.alias,"message":message.id}),
        );
        tx.execute("UPDATE app_run_steps SET state='succeeded',result_digest=? WHERE run_id=? AND step_id=?",params![result_digest,run_id,step_id])?;
        tx.execute(
            "UPDATE tasks SET state='done',updated=? WHERE id=?",
            params![now(), task_id],
        )?;
        let pending: i64 = tx.query_row(
            "SELECT COUNT(*) FROM app_run_steps WHERE run_id=? AND state!='succeeded'",
            [&run_id],
            |r| r.get(0),
        )?;
        if pending == 0 {
            tx.execute(
                "UPDATE app_runs SET state='succeeded',updated=? WHERE id=?",
                params![now(), run_id],
            )?;
            tx.execute(
                "UPDATE jobs SET state='done',updated=? WHERE id=?",
                params![now(), run_id],
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
            json!({"run_id":run_id,"step_id":step_id,"result_digest":result_digest,"producer":message.alias,"eligible_successors":pending>0}),
        )?;
        Ok(true)
    }
}
