//! Persist reviewed-artifact decisions and the reservation for a selected ticket.
//! The daemon owns the tracker write; these operations stay in Store transactions.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::{artifact_digest, human_receipt, APP_RUN_APPROVAL_STREAM};
use crate::error::{Error, Result};
use crate::store::{now, Store, StoreConn};

/// CAD-1887: proposals under the meeting skill's `## Proposed next steps`
/// heading. The heading match is exact. A later heading ends the section.
/// Anything else in the section must be one bullet, so a prose line cannot
/// be dropped on the way to a ticket. At most 64 proposals.
pub(in crate::store) fn parse_proposed_steps(text: &str) -> Result<Vec<String>> {
    let mut sections = 0usize;
    let mut items = Vec::new();
    let mut in_section = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            if line == "## Proposed next steps" {
                sections += 1;
                if sections > 1 {
                    return Err(Error::rejected(
                        "artifact has more than one Proposed next steps section",
                    ));
                }
                in_section = true;
            } else if in_section {
                in_section = false;
            }
            continue;
        }
        if !in_section || line.is_empty() {
            continue;
        }
        let Some(item) = proposal_line(line) else {
            return Err(Error::rejected(
                "Proposed next steps contains a line that is not a single proposal",
            ));
        };
        if item.is_empty() {
            return Err(Error::rejected(
                "Proposed next steps contains an empty proposal",
            ));
        }
        items.push(item);
        if items.len() > 64 {
            return Err(Error::rejected("artifact has too many proposals"));
        }
    }
    if sections == 0 {
        return Err(Error::rejected(
            "artifact has no Proposed next steps section",
        ));
    }
    Ok(items)
}

fn proposal_line(line: &str) -> Option<String> {
    let rest = if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        rest
    } else {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == 0 || i >= bytes.len() {
            return None;
        }
        if bytes[i] != b'.' && bytes[i] != b')' {
            return None;
        }
        line[i + 1..].strip_prefix(' ')?
    };
    Some(rest.trim().to_string())
}

fn decision_artifact_id(id: &str) -> Result<()> {
    if !id.is_empty()
        && id.len() <= 160
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(Error::rejected(
            "artifact id is not a stored app artifact id",
        ))
    }
}

fn decision_digest(digest: &str) -> Result<()> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(Error::rejected("operator decision needs a sha256 digest"));
    };
    if hex.len() == 64
        && hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(Error::rejected("operator decision needs a sha256 digest"))
    }
}

struct StoredDisposition {
    run_id: String,
    request_id: String,
    artifact_id: String,
    artifact_digest: String,
    decision: String,
    created: f64,
}

struct StoredTicket {
    run_id: String,
    request_id: String,
    artifact_id: String,
    artifact_digest: String,
    index: i64,
    count: i64,
    text: String,
    issue_id: Option<String>,
    created: f64,
}

fn disposition_value(row: &StoredDisposition, replayed: bool) -> Value {
    json!({
        "run_id": row.run_id,
        "request_id": row.request_id,
        "artifact_id": row.artifact_id,
        "artifact_digest": row.artifact_digest,
        "decision": row.decision,
        "created": row.created as i64,
        "replayed": replayed,
    })
}

fn ticket_value(row: &StoredTicket, replayed: bool) -> Value {
    json!({
        "run_id": row.run_id,
        "request_id": row.request_id,
        "artifact_id": row.artifact_id,
        "artifact_digest": row.artifact_digest,
        "index": row.index,
        "count": row.count,
        "suggestion": row.text,
        "issue_id": row.issue_id,
        "created": row.created as i64,
        "replayed": replayed,
    })
}

impl Store {
    /// Accept or decline one reviewed artifact. This does not stage, publish,
    /// or touch the effect ledger. One row per run; the same request replays.
    pub fn app_run_disposition(
        &self,
        run_id: &str,
        request_id: &str,
        artifact_id: &str,
        digest: &str,
        decision: &str,
        human: Option<&Value>,
    ) -> Result<Value> {
        let run_id = crate::proto::identifier(run_id, "app run ID")?;
        let request_id = crate::proto::identifier(request_id, "app run request ID")?;
        decision_artifact_id(artifact_id)?;
        decision_digest(digest)?;
        if !matches!(decision, "accept" | "decline") {
            return Err(Error::rejected("operator decision is accept or decline"));
        }
        let operation = uuid::Uuid::new_v4().to_string();
        self.write_tx(|conn| {
            let tx = &mut *conn;
            Self::require_reviewed_artifact(&tx, &run_id, artifact_id, digest)?;
            if let Some(row) = Self::disposition_by_run(&tx, &run_id)? {
                if row.request_id == request_id
                    && row.artifact_id == artifact_id
                    && row.artifact_digest == digest
                    && row.decision == decision
                {
                    return Ok(disposition_value(&row, true));
                }
                return Err(Error::rejected(if row.request_id == request_id {
                    "this request id already records a different operator decision"
                } else {
                    "this run already has an operator decision"
                }));
            }
            if Self::disposition_by_request(&tx, &request_id)?.is_some() {
                return Err(Error::rejected(
                    "this request id already records a different operator decision",
                ));
            }
            tx.execute(
                "INSERT INTO app_run_dispositions(run_id,request_id,artifact_id,artifact_digest,decision,created) VALUES(?,?,?,?,?,?)",
                params![run_id, request_id, artifact_id, digest, decision, now()],
            )?;
            Self::event_scoped(
                &tx,
                APP_RUN_APPROVAL_STREAM,
                "app_run_disposition_recorded",
                human_receipt(
                    json!({
                        "run_id": run_id,
                        // CAD-1374 owns `request_id` on a human receipt: it
                        // is the operation UUID. The caller's idempotency
                        // id is kept beside it.
                        "decision_request_id": request_id,
                        "artifact_id": artifact_id,
                        "digest": digest,
                        "decision": decision,
                        "by": "operator",
                    }),
                    human,
                    &operation,
                    "app_run_disposition",
                    &format!("app-run:{run_id}"),
                    decision,
                ),
                Some(&run_id),
                None,
            )?;
            let row = Self::disposition_by_run(&tx, &run_id)?
                .ok_or_else(|| Error::internal("operator decision was not stored"))?;
            Ok(disposition_value(&row, false))
        })
    }

    /// Reserve the one selected proposal. The artifact text is parsed here so
    /// the issue writer cannot substitute a different suggestion.
    pub fn app_run_ticket_reserve(
        &self,
        run_id: &str,
        request_id: &str,
        artifact_id: &str,
        digest: &str,
        index: u32,
        human: Option<&Value>,
    ) -> Result<Value> {
        let run_id = crate::proto::identifier(run_id, "app run ID")?;
        let request_id = crate::proto::identifier(request_id, "app run request ID")?;
        decision_artifact_id(artifact_id)?;
        decision_digest(digest)?;
        if index == 0 || index > 64 {
            return Err(Error::rejected(
                "suggestion index is outside the proposal list",
            ));
        }
        let operation = uuid::Uuid::new_v4().to_string();
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let text = Self::require_reviewed_artifact(&tx, &run_id, artifact_id, digest)?;
            let disposition = Self::disposition_by_run(&tx, &run_id)?.ok_or_else(|| {
                Error::rejected("ticket needs an accepted disposition for this artifact")
            })?;
            if disposition.decision != "accept"
                || disposition.artifact_id != artifact_id
                || disposition.artifact_digest != digest
            {
                return Err(Error::rejected(
                    "ticket needs an accepted disposition for this artifact",
                ));
            }
            let steps = parse_proposed_steps(&text)?;
            if steps.len() < 2 {
                return Err(Error::rejected(
                    "a selected ticket needs at least two proposals",
                ));
            }
            if index as usize > steps.len() {
                return Err(Error::rejected(
                    "suggestion index is outside the proposal list",
                ));
            }
            let suggestion = &steps[index as usize - 1];
            if let Some(other) = Self::ticket_by_request(&tx, &request_id)? {
                if other.run_id != run_id {
                    return Err(Error::rejected(
                        "this request id already records a different ticket",
                    ));
                }
            }
            if let Some(row) = Self::ticket_by_run(&tx, &run_id)? {
                let same = row.request_id == request_id
                    && row.artifact_id == artifact_id
                    && row.artifact_digest == digest
                    && row.index == i64::from(index)
                    && row.count == steps.len() as i64
                    && row.text == *suggestion;
                if same {
                    return Ok(ticket_value(&row, true));
                }
                return Err(Error::rejected(if row.request_id == request_id {
                    "this request id already records a different ticket"
                } else {
                    "this run already has a selected ticket"
                }));
            }
            tx.execute(
                "INSERT INTO app_run_tickets(run_id,request_id,artifact_id,artifact_digest,suggestion_index,suggestion_count,suggestion_text,issue_id,created) VALUES(?,?,?,?,?,?,?,NULL,?)",
                params![
                    run_id,
                    request_id,
                    artifact_id,
                    digest,
                    i64::from(index),
                    steps.len() as i64,
                    suggestion,
                    now()
                ],
            )?;
            Self::event_scoped(
                &tx,
                APP_RUN_APPROVAL_STREAM,
                "app_run_ticket_reserved",
                human_receipt(
                    json!({
                        "run_id": run_id,
                        "decision_request_id": request_id,
                        "artifact_id": artifact_id,
                        "digest": digest,
                        "index": index,
                        "count": steps.len(),
                        "by": "operator",
                    }),
                    human,
                    &operation,
                    "app_run_ticket_reserve",
                    &format!("app-run:{run_id}"),
                    "reserved",
                ),
                Some(&run_id),
                None,
            )?;
            let row = Self::ticket_by_run(&tx, &run_id)?
                .ok_or_else(|| Error::internal("selected ticket was not stored"))?;
            Ok(ticket_value(&row, false))
        })
    }

    /// Attach the tracker issue created for a reservation. The same issue
    /// replays; a second issue for the same run or request conflicts.
    ///
    /// Syntax, the reservation pair, and uniqueness are the whole check
    /// here. This does not read the tracker. The daemon bind path confirms
    /// the id is the committed selected issue before it calls this.
    pub fn app_run_ticket_bind(
        &self,
        run_id: &str,
        request_id: &str,
        issue_id: &str,
        human: Option<&Value>,
    ) -> Result<Value> {
        let run_id = crate::proto::identifier(run_id, "app run ID")?;
        let request_id = crate::proto::identifier(request_id, "app run request ID")?;
        let issue_id = crate::issue::model::check_id(issue_id)?;
        let operation = uuid::Uuid::new_v4().to_string();
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let Some(row) = Self::ticket_by_run(&tx, &run_id)? else {
                return Err(Error::rejected("ticket bind needs the reserved request"));
            };
            if row.request_id != request_id {
                return Err(Error::rejected("ticket bind needs the reserved request"));
            }
            if let Some(bound) = &row.issue_id {
                if bound == &issue_id {
                    return Ok(ticket_value(&row, true));
                }
                return Err(Error::rejected(
                    "ticket is already bound to a different issue",
                ));
            }
            if let Some(owner) = tx
                .query_row(
                    "SELECT run_id FROM app_run_tickets WHERE issue_id=?",
                    [&issue_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
            {
                if owner != run_id {
                    return Err(Error::rejected(
                        "that issue is already bound to another run",
                    ));
                }
            }
            let updated = tx.execute(
                "UPDATE app_run_tickets SET issue_id=? WHERE run_id=? AND request_id=? AND issue_id IS NULL",
                params![issue_id, run_id, request_id],
            )?;
            if updated != 1 {
                return Err(Error::rejected("ticket bind needs the reserved request"));
            }
            Self::event_scoped(
                &tx,
                APP_RUN_APPROVAL_STREAM,
                "app_run_ticket_bound",
                human_receipt(
                    json!({
                        "run_id": run_id,
                        "decision_request_id": request_id,
                        "issue_id": issue_id,
                        "digest": row.artifact_digest,
                        "by": "operator",
                    }),
                    human,
                    &operation,
                    "app_run_ticket_bind",
                    &format!("app-run:{run_id}"),
                    "bound",
                ),
                Some(&run_id),
                None,
            )?;
            let row = Self::ticket_by_run(&tx, &run_id)?
                .ok_or_else(|| Error::internal("selected ticket was not stored"))?;
            Ok(ticket_value(&row, false))
        })
    }

    pub(super) fn operator_disposition_json(conn: &impl StoreConn, run_id: &str) -> Result<Value> {
        Ok(match Self::disposition_by_run(conn, run_id)? {
            None => Value::Null,
            Some(row) => json!({
                "request_id": row.request_id,
                "artifact_id": row.artifact_id,
                "artifact_digest": row.artifact_digest,
                "decision": row.decision,
                "created": row.created as i64,
            }),
        })
    }

    pub(super) fn operator_ticket_json(conn: &impl StoreConn, run_id: &str) -> Result<Value> {
        Ok(match Self::ticket_by_run(conn, run_id)? {
            None => Value::Null,
            Some(row) => json!({
                "request_id": row.request_id,
                "artifact_id": row.artifact_id,
                "artifact_digest": row.artifact_digest,
                "index": row.index,
                "count": row.count,
                "suggestion": row.text,
                "issue_id": row.issue_id,
                "created": row.created as i64,
            }),
        })
    }

    fn require_reviewed_artifact(
        conn: &impl StoreConn,
        run_id: &str,
        artifact_id: &str,
        digest: &str,
    ) -> Result<String> {
        let run = Self::app_run_show_in(conn, run_id)?;
        if run["state"] != "succeeded" || run["approved_digest"] != run["snapshot_digest"] {
            return Err(Error::rejected(
                "operator decision needs a succeeded approved run",
            ));
        }
        let (stored_digest, media, producer, bytes): (String, String, String, Vec<u8>) = conn
            .query_row(
                "SELECT digest, media_type, producer, content FROM app_run_artifacts WHERE id=? AND run_id=?",
                params![artifact_id, run_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| Error::rejected("artifact is not part of this run"))?;
        if stored_digest != digest
            || artifact_digest(&bytes) != digest
            || !matches!(media.as_str(), "text/plain" | "text/markdown")
        {
            return Err(Error::rejected(
                "artifact digest does not match the stored bytes",
            ));
        }
        let reviews: Vec<(String, String, String)> = conn.query_vec(
            "SELECT reviewer, decision, artifact_digest FROM app_run_reviews WHERE run_id=? AND artifact_id=?",
            params![run_id, artifact_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut independent = false;
        for (reviewer, decision, review_digest) in reviews {
            if decision == "approve" && reviewer == producer {
                return Err(Error::rejected(
                    "producer approval is not an independent review",
                ));
            }
            if decision == "approve" && review_digest == digest && reviewer != producer {
                independent = true;
            }
        }
        if !independent {
            return Err(Error::rejected(
                "reviewed artifact needs an independent approval",
            ));
        }
        String::from_utf8(bytes).map_err(|_| Error::rejected("stored text artifact is not UTF-8"))
    }

    fn disposition_by_run(
        conn: &impl StoreConn,
        run_id: &str,
    ) -> Result<Option<StoredDisposition>> {
        conn.query_row(
            "SELECT run_id, request_id, artifact_id, artifact_digest, decision, created FROM app_run_dispositions WHERE run_id=?",
            [run_id],
            |r| {
                Ok(StoredDisposition {
                    run_id: r.get(0)?,
                    request_id: r.get(1)?,
                    artifact_id: r.get(2)?,
                    artifact_digest: r.get(3)?,
                    decision: r.get(4)?,
                    created: r.get(5)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    fn disposition_by_request(
        conn: &impl StoreConn,
        request_id: &str,
    ) -> Result<Option<StoredDisposition>> {
        conn.query_row(
            "SELECT run_id, request_id, artifact_id, artifact_digest, decision, created FROM app_run_dispositions WHERE request_id=?",
            [request_id],
            |r| {
                Ok(StoredDisposition {
                    run_id: r.get(0)?,
                    request_id: r.get(1)?,
                    artifact_id: r.get(2)?,
                    artifact_digest: r.get(3)?,
                    decision: r.get(4)?,
                    created: r.get(5)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    fn ticket_by_run(conn: &impl StoreConn, run_id: &str) -> Result<Option<StoredTicket>> {
        Self::ticket_row(
            conn,
            "SELECT run_id, request_id, artifact_id, artifact_digest, suggestion_index, suggestion_count, suggestion_text, issue_id, created FROM app_run_tickets WHERE run_id=?",
            run_id,
        )
    }

    fn ticket_by_request(conn: &impl StoreConn, request_id: &str) -> Result<Option<StoredTicket>> {
        Self::ticket_row(
            conn,
            "SELECT run_id, request_id, artifact_id, artifact_digest, suggestion_index, suggestion_count, suggestion_text, issue_id, created FROM app_run_tickets WHERE request_id=?",
            request_id,
        )
    }

    fn ticket_row(conn: &impl StoreConn, sql: &str, key: &str) -> Result<Option<StoredTicket>> {
        conn.query_row(sql, [key], |r| {
            Ok(StoredTicket {
                run_id: r.get(0)?,
                request_id: r.get(1)?,
                artifact_id: r.get(2)?,
                artifact_digest: r.get(3)?,
                index: r.get(4)?,
                count: r.get(5)?,
                text: r.get(6)?,
                issue_id: r.get(7)?,
                created: r.get(8)?,
            })
        })
        .optional()
        .map_err(Into::into)
    }
}
