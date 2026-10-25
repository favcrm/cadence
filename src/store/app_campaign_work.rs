//! CAD-1827: campaign work rows in the installation's record file.
//!
//! The CRM `crm-campaign-work/v1` record is installation/context-scoped
//! data, so it lives beside customer records in
//! `<state_dir>/app-records/<install_id>.sqlite3`, keyed
//! `(context_id, campaign_id)`. Contexts share the file and never see
//! each other's rows; installations never share a file. The rules
//! themselves — value grammar, attribution, revision application, the
//! read shape — live in [`crate::issue::app_campaign_work`]; this module
//! only persists one row and runs the CAS inside one transaction.
//!
//! The write is one `BEGIN IMMEDIATE`: the current row, the live content
//! revision and the caller-derived attribution are read and written
//! together, so a concurrent write, a content save or a stale
//! `expected_revision` cannot interleave between the check and the
//! commit. A stale token refuses with the contract's
//! `stale_revision` code; there is no silent retry, overwrite or merge.
//!
//! No new database and no file-schema bump: the table is additive
//! (`CREATE TABLE IF NOT EXISTS`) and an older file gains it on open.

use super::app_records::RecordStore;
use super::*;
use crate::issue::app_campaign_work::{
    self, Access, ContentFacts, ReadInput, WorkAttribution, WorkPatch, WorkRecord, WorkStatus,
};
use rusqlite::params;
use serde_json::Value;

const WORK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS app_campaign_work(\
 context_id TEXT NOT NULL, campaign_id TEXT NOT NULL,\
 revision INTEGER NOT NULL CHECK(revision>0),\
 content_revision INTEGER CHECK(content_revision IS NULL OR content_revision>0),\
 assignee TEXT, assigned_by TEXT, assigned_at REAL,\
 due_at TEXT,\
 status TEXT CHECK(status IS NULL OR status IN ('planned','in_progress','blocked','done','cancelled')),\
 blocked_reason TEXT,\
 actor TEXT NOT NULL, updated REAL NOT NULL,\
 PRIMARY KEY(context_id, campaign_id));\
CREATE INDEX IF NOT EXISTS app_campaign_work_context ON app_campaign_work(context_id, updated);";

/// Add the campaign work table to an opened record file. Additive on
/// every open: a file written by an earlier build gains the table at the
/// current file schema, and its rows start empty — never invented.
pub(crate) fn ensure_schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(WORK_SCHEMA)
        .map_err(|e| Error::internal(format!("campaign work schema unavailable: {e}")))
}

/// One stored row before it becomes a [`WorkRecord`].
type StoredRow = (
    i64,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<f64>,
    Option<String>,
    Option<String>,
    Option<String>,
    f64,
);

fn stored_record(row: StoredRow) -> Result<WorkRecord> {
    let status = match row.6.as_deref() {
        None => None,
        Some(text) => Some(
            WorkStatus::parse(text)
                .ok_or_else(|| Error::internal("campaign work status is not a known state"))?,
        ),
    };
    Ok(WorkRecord {
        revision: row.0,
        content_revision: row.1,
        assignee: row.2,
        assigned_by: row.3,
        assigned_at: row.4.map(app_campaign_work::instant).transpose()?,
        due_at: row.5,
        status,
        blocked_reason: row.7,
        updated_at: app_campaign_work::instant(row.8)?,
    })
}

/// An ISO-8601 instant back to the record file's epoch-seconds column.
fn epoch(stamp: &str) -> Result<f64> {
    let parsed = chrono::DateTime::parse_from_rfc3339(stamp)
        .map_err(|_| Error::internal("campaign work timestamp is not an instant"))?;
    Ok(parsed.timestamp_millis() as f64 / 1000.0)
}

fn checked_scope(context: &str, campaign: &str) -> Result<()> {
    crate::proto::identifier(context, "context ID")?;
    app_campaign_work::check_campaign_id(campaign)
}

impl RecordStore {
    fn campaign_work_record(
        conn: &impl super::StoreConn,
        context: &str,
        campaign: &str,
    ) -> Result<Option<WorkRecord>> {
        let row: Option<StoredRow> = conn
            .query_opt(
                "SELECT revision,content_revision,assignee,assigned_by,assigned_at,due_at,status,blocked_reason,updated \
                 FROM app_campaign_work WHERE context_id=? AND campaign_id=?",
                params![context, campaign],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        row.map(stored_record).transpose()
    }

    /// The live content revision this campaign holds, or `None` when no
    /// content is saved yet. Read inside the caller's transaction so a
    /// write binds the revision it really saw.
    fn campaign_content(
        conn: &impl super::StoreConn,
        context: &str,
        campaign: &str,
    ) -> Result<Option<ContentFacts>> {
        let row: Option<(i64, String)> = conn
            .query_opt(
                "SELECT revision,content_digest FROM app_content_docs WHERE context_id=? AND campaign_id=?",
                params![context, campaign],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        Ok(row.map(|(revision, digest)| ContentFacts { revision, digest }))
    }

    /// The recorded facts for `campaign-work.show`: this scope's work row
    /// (or none) and the content revision it references. Never derived,
    /// never filled from another campaign or from content authorship.
    pub fn app_campaign_work_facts(
        &self,
        context: &str,
        campaign: &str,
        app: &str,
    ) -> Result<Value> {
        checked_scope(context, campaign)?;
        let conn = self.conn();
        let work = Self::campaign_work_record(&conn, context, campaign)?;
        let content = Self::campaign_content(&conn, context, campaign)?;
        Ok(app_campaign_work::facts_json(
            app,
            self.install(),
            Some(context),
            campaign,
            work.as_ref(),
            content.as_ref(),
        ))
    }

    /// The caller-facing read for an authorized caller: the recorded facts
    /// projected through the contract's state rules. The caller proves
    /// `access` and its scope first; a forbidden caller reads no row at all
    /// (and does not create the file), so a refusal can never leak values.
    pub fn app_campaign_work_read(
        &self,
        context: &str,
        campaign: &str,
        app: &str,
        access: Access,
        expected_revision: Option<i64>,
    ) -> Result<Value> {
        checked_scope(context, campaign)?;
        if access != Access::Authorized {
            return Ok(app_campaign_work::read(ReadInput {
                app,
                install: self.install(),
                context: Some(context),
                campaign,
                work: None,
                content: None,
                readiness: None,
                access,
                expected_revision: None,
            }));
        }
        let conn = self.conn();
        let work = Self::campaign_work_record(&conn, context, campaign)?;
        let content = Self::campaign_content(&conn, context, campaign)?;
        Ok(app_campaign_work::read(ReadInput {
            app,
            install: self.install(),
            context: Some(context),
            campaign,
            work: work.as_ref(),
            content: content.as_ref(),
            // The host cannot read the CRM readiness chain, so it passes no
            // answers and the read is labelled partial rather than guessed.
            readiness: None,
            access,
            expected_revision,
        }))
    }

    /// Record assignment, due date, status and blocked reason under one
    /// revision CAS. `actor` is the daemon-derived authenticated caller;
    /// nothing from the request body can stand in for it.
    pub fn app_campaign_work_save(
        &self,
        context: &str,
        campaign: &str,
        expected_revision: i64,
        patch: &WorkPatch,
        actor: &str,
        app: &str,
    ) -> Result<Value> {
        checked_scope(context, campaign)?;
        self.write_tx(|tx| {
            let current = Self::campaign_work_record(tx, context, campaign)?;
            if expected_revision != WorkRecord::cas_revision(current.as_ref()) {
                return Err(Error::conflict_coded(
                    "stale_revision",
                    "These campaign work details changed. Reload them before saving again.",
                ));
            }
            let content = Self::campaign_content(tx, context, campaign)?;
            let attribution = WorkAttribution {
                actor: actor.to_string(),
                at: app_campaign_work::instant(now())?,
            };
            let next = app_campaign_work::apply(
                current,
                patch,
                &attribution,
                content.as_ref().map(|content| content.revision),
            )?;
            tx.execute(
                "INSERT INTO app_campaign_work(context_id,campaign_id,revision,content_revision,assignee,assigned_by,assigned_at,due_at,status,blocked_reason,actor,updated) \
                 VALUES(?,?,?,?,?,?,?,?,?,?,?,?) \
                 ON CONFLICT(context_id,campaign_id) DO UPDATE SET revision=excluded.revision, content_revision=excluded.content_revision, assignee=excluded.assignee, assigned_by=excluded.assigned_by, assigned_at=excluded.assigned_at, due_at=excluded.due_at, status=excluded.status, blocked_reason=excluded.blocked_reason, actor=excluded.actor, updated=excluded.updated",
                params![
                    context,
                    campaign,
                    next.revision,
                    next.content_revision,
                    next.assignee,
                    next.assigned_by,
                    next.assigned_at.as_deref().map(epoch).transpose()?,
                    next.due_at,
                    next.status.map(WorkStatus::as_str),
                    next.blocked_reason,
                    actor,
                    epoch(&next.updated_at)?,
                ],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            Ok(app_campaign_work::facts_json(
                app,
                self.install(),
                Some(context),
                campaign,
                Some(&next),
                content.as_ref(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::app_content::Draft;
    use serde_json::json;
    use tempfile::TempDir;

    const CONTEXT: &str = "ctx-a";
    const OTHER_CONTEXT: &str = "ctx-b";
    const CAMPAIGN: &str = "welcome";

    fn store() -> (TempDir, RecordStore) {
        let dir = TempDir::new().unwrap();
        let store = RecordStore::open(dir.path(), "install-a").unwrap();
        (dir, store)
    }

    fn patch(fields: Value) -> WorkPatch {
        WorkPatch::from_body(&fields).unwrap()
    }

    fn content(store: &RecordStore, context: &str, campaign: &str) -> i64 {
        let draft = Draft::parse(
            "Spring launch",
            "Short preheader",
            &[json!({"type": "paragraph", "text": "Hello"})],
        )
        .unwrap();
        let current = store
            .app_content_show(context, campaign)
            .ok()
            .and_then(|shown| shown["content"]["revision"].as_i64());
        store
            .app_content_save(context, campaign, current, &draft)
            .unwrap()["content"]["revision"]
            .as_i64()
            .unwrap()
    }

    #[test]
    fn first_write_takes_revision_zero_then_the_cas_token_advances() {
        let (_dir, store) = store();
        let saved = store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                0,
                &patch(json!({"assignee": "Ada", "due_at": "2026-11-01"})),
                "operator",
                "crm",
            )
            .unwrap();
        assert_eq!(saved["work"]["revision"], 1);
        assert_eq!(saved["work"]["assignee"], "Ada");
        assert_eq!(saved["work"]["assigned_by"], "operator");
        assert_eq!(saved["work"]["due_at"], "2026-11-01");
        assert_eq!(saved["contract"], app_campaign_work::CONTRACT);
        assert_eq!(saved["source"]["install_id"], "install-a");
        assert_eq!(saved["source"]["context_id"], CONTEXT);
        assert_eq!(saved["campaign_id"], CAMPAIGN);

        // A replay of the first token is stale and writes nothing.
        let stale = store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                0,
                &patch(json!({"due_at": "2026-12-01"})),
                "operator",
                "crm",
            )
            .unwrap_err();
        assert_eq!(stale.code(), Some("stale_revision"));
        assert_eq!(stale.kind(), "conflict");
        let unchanged = store
            .app_campaign_work_facts(CONTEXT, CAMPAIGN, "crm")
            .unwrap();
        assert_eq!(unchanged["work"]["revision"], 1);
        assert_eq!(unchanged["work"]["due_at"], "2026-11-01");

        let next = store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                1,
                &patch(json!({"due_at": "2026-12-01"})),
                "agent",
                "crm",
            )
            .unwrap();
        assert_eq!(next["work"]["revision"], 2);
        assert_eq!(next["work"]["due_at"], "2026-12-01");
        // A due-date-only write keeps the assignment provenance.
        assert_eq!(next["work"]["assigned_by"], "operator");
        assert_eq!(next["work"]["assigned_at"], saved["work"]["assigned_at"]);
        assert_ne!(next["work"]["updated_at"], saved["work"]["updated_at"]);
    }

    #[test]
    fn each_context_and_campaign_holds_its_own_row() {
        let (_dir, store) = store();
        store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                0,
                &patch(json!({"assignee": "Ada"})),
                "operator",
                "crm",
            )
            .unwrap();
        // Same install, another context: nothing recorded, token 0.
        let other_context = store
            .app_campaign_work_facts(OTHER_CONTEXT, CAMPAIGN, "crm")
            .unwrap();
        assert_eq!(other_context["work"], Value::Null);
        // Same context, another campaign: nothing recorded either.
        let other_campaign = store
            .app_campaign_work_facts(CONTEXT, "second", "crm")
            .unwrap();
        assert_eq!(other_campaign["work"], Value::Null);
        // The second context's first write starts at 0, not at the sibling's 1.
        let saved = store
            .app_campaign_work_save(
                OTHER_CONTEXT,
                CAMPAIGN,
                0,
                &patch(json!({"assignee": "Grace"})),
                "operator",
                "crm",
            )
            .unwrap();
        assert_eq!(saved["work"]["revision"], 1);
        assert_eq!(saved["work"]["assignee"], "Grace");
        assert_eq!(
            store
                .app_campaign_work_facts(CONTEXT, CAMPAIGN, "crm")
                .unwrap()["work"]["assignee"],
            "Ada"
        );
    }

    #[test]
    fn a_write_binds_the_content_revision_it_saw() {
        let (_dir, store) = store();
        let first_content = content(&store, CONTEXT, CAMPAIGN);
        let saved = store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                0,
                &patch(json!({"assignee": "Ada"})),
                "operator",
                "crm",
            )
            .unwrap();
        assert_eq!(saved["content"]["revision"], first_content);
        assert_eq!(saved["work"]["content_revision"], first_content);

        let second_content = content(&store, CONTEXT, CAMPAIGN);
        assert_ne!(second_content, first_content);
        let rebound = store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                1,
                &patch(json!({"due_at": "2026-11-02"})),
                "operator",
                "crm",
            )
            .unwrap();
        assert_eq!(rebound["work"]["content_revision"], second_content);
        assert_eq!(rebound["content"]["revision"], second_content);
        // A campaign with no content saves work with no content binding.
        let bare = store
            .app_campaign_work_save(
                CONTEXT,
                "no-content",
                0,
                &patch(json!({"assignee": "Ada"})),
                "operator",
                "crm",
            )
            .unwrap();
        assert_eq!(bare["content"], Value::Null);
        assert_eq!(bare["work"]["content_revision"], Value::Null);
    }

    #[test]
    fn blocked_reason_rules_hold_at_the_store_boundary() {
        let (_dir, store) = store();
        // A reason without the blocked status refuses before any row exists.
        let orphan = store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                0,
                &patch(json!({"blocked_reason": "no owner"})),
                "operator",
                "crm",
            )
            .unwrap_err();
        assert_eq!(orphan.kind(), "rejected");
        assert_eq!(
            store
                .app_campaign_work_facts(CONTEXT, CAMPAIGN, "crm")
                .unwrap()["work"],
            Value::Null
        );

        store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                0,
                &patch(json!({"status": "blocked", "blocked_reason": "waiting on legal"})),
                "operator",
                "crm",
            )
            .unwrap();
        // A reason-only write must still carry the blocked status itself;
        // the live row's status is not a claim this request made.
        let omitted = store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                1,
                &patch(json!({"blocked_reason": "waiting on tax"})),
                "operator",
                "crm",
            )
            .unwrap_err();
        assert_eq!(omitted.kind(), "rejected");
        assert_eq!(
            store
                .app_campaign_work_facts(CONTEXT, CAMPAIGN, "crm")
                .unwrap()["work"]["revision"],
            1,
            "an omitted-status reason changed nothing"
        );

        // A reason-only edit of an already-blocked row is a valid CAS write.
        let reason_only = store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                1,
                &patch(json!({"status": "blocked", "blocked_reason": "waiting on finance"})),
                "operator",
                "crm",
            )
            .unwrap();
        assert_eq!(reason_only["work"]["revision"], 2);
        assert_eq!(reason_only["work"]["blocked_reason"], "waiting on finance");
        // Resuming clears the block text.
        let resumed = store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                2,
                &patch(json!({"status": "in_progress"})),
                "operator",
                "crm",
            )
            .unwrap();
        assert_eq!(resumed["work"]["status"], "in_progress");
        assert_eq!(resumed["work"]["blocked_reason"], Value::Null);
    }

    #[test]
    fn unassigning_is_an_attributed_decision() {
        let (_dir, store) = store();
        let assigned = store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                0,
                &patch(json!({"assignee": "Ada"})),
                "operator",
                "crm",
            )
            .unwrap();
        let cleared = store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                1,
                &patch(json!({"assignee": null})),
                "agent",
                "crm",
            )
            .unwrap();
        assert_eq!(cleared["work"]["assignee"], Value::Null);
        assert_eq!(cleared["work"]["assigned_by"], "agent");
        assert_ne!(
            cleared["work"]["assigned_at"],
            assigned["work"]["assigned_at"]
        );
        let read = store
            .app_campaign_work_read(CONTEXT, CAMPAIGN, "crm", Access::Authorized, None)
            .unwrap();
        assert_eq!(read["state"], "partial");
        assert_eq!(read["next_decision"], Value::Null);
        assert!(read["unavailable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "assignee"));
    }

    #[test]
    fn the_read_refuses_forbidden_and_stale_without_values() {
        let (_dir, store) = store();
        store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                0,
                &patch(json!({"assignee": "Ada", "status": "blocked", "blocked_reason": "legal"})),
                "operator",
                "crm",
            )
            .unwrap();
        // Forbidden reads no row at all — the shape carries no values.
        let forbidden = store
            .app_campaign_work_read(OTHER_CONTEXT, CAMPAIGN, "crm", Access::Forbidden, None)
            .unwrap();
        assert_eq!(forbidden["state"], "forbidden");
        assert_eq!(forbidden["refusal"]["code"], "forbidden");
        assert_eq!(forbidden["revision"], Value::Null);
        assert_eq!(forbidden["assignment"], Value::Null);

        let stale = store
            .app_campaign_work_read(CONTEXT, CAMPAIGN, "crm", Access::Authorized, Some(0))
            .unwrap();
        assert_eq!(stale["state"], "stale");
        assert_eq!(stale["refusal"]["code"], "stale_revision");
        assert_eq!(stale["revision"], 1);
        assert_eq!(stale["content_revision"], Value::Null);
        assert_eq!(stale["assignment"], Value::Null);

        let current = store
            .app_campaign_work_read(CONTEXT, CAMPAIGN, "crm", Access::Authorized, Some(1))
            .unwrap();
        // The host cannot read the CRM readiness chain, so the decision is
        // suppressed (partial) even though the row is blocked — the app
        // gates `nextDecision` on readiness, and the host never guesses.
        assert_eq!(current["state"], "partial");
        assert_eq!(current["identity"], "crm:install-a:ctx-a:welcome");
        assert_eq!(current["assignment"]["assignee"], "Ada");
        assert_eq!(current["next_decision"], Value::Null);
        // The decision and the unrecorded due date are the only gaps.
        assert_eq!(current["unavailable"], json!(["next_decision", "due_at"]));
    }

    #[test]
    fn the_work_table_is_additive_on_an_existing_file() {
        let (dir, store) = store();
        store
            .app_campaign_work_save(
                CONTEXT,
                CAMPAIGN,
                0,
                &patch(json!({"assignee": "Ada"})),
                "operator",
                "crm",
            )
            .unwrap();
        drop(store);
        // Reopening re-runs the additive schema and keeps the file schema.
        let reopened = RecordStore::open(dir.path(), "install-a").unwrap();
        assert_eq!(
            reopened
                .app_campaign_work_facts(CONTEXT, CAMPAIGN, "crm")
                .unwrap()["work"]["assignee"],
            "Ada"
        );
        let version: i64 = reopened
            .conn()
            .query_row("SELECT version FROM record_schema", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, crate::store::app_records::FILE_SCHEMA);
    }
}
