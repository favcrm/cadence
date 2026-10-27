//! CAD-914: resumable wiki transfer jobs in the daemon Store.
//!
//! Job identity is the server-generated id plus the manifest digest and the
//! daemon-derived owner key. The owner key is a hash of the connection
//! caller and, when the operator board relayed one, the verified envelope
//! issuer, organization, audience and subject. Client identity fields never
//! enter that key. Staged bytes are recorded only after they are durable.
//! A publication attempt is recorded before the canonical wiki writer, so a
//! crash cannot look like a fresh pending entry. The implicit manifest root
//! has its own attempt fence on the job and is not stored in an unrelated
//! column. Resume may reconcile a committed match; it does not publish an
//! unresolved attempt again.

use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Store, StoreConn};
use crate::error::{Error, Result};

/// Schema v49. New tables only; nothing sealed or witnessed is altered.
/// `root_attempt` is the implicit-root fence: `fresh`, `unresolved`, or
/// `proved`. Entry outcome `unresolved` is the per-entry publication fence.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS wiki_transfer_jobs(
    job_id TEXT PRIMARY KEY,
    digest TEXT NOT NULL,
    manifest_json TEXT NOT NULL,
    source_id TEXT NOT NULL,
    root TEXT NOT NULL,
    owner_key TEXT NOT NULL,
    state TEXT NOT NULL,
    root_attempt TEXT NOT NULL DEFAULT 'fresh',
    index_kicked INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS wiki_transfer_entries(
    job_id TEXT NOT NULL REFERENCES wiki_transfer_jobs(job_id),
    entry_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    path TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    kind TEXT NOT NULL,
    size INTEGER NOT NULL,
    sha256 TEXT,
    if_rev TEXT NOT NULL,
    received INTEGER NOT NULL DEFAULT 0,
    outcome TEXT NOT NULL,
    rev TEXT,
    extraction TEXT,
    extraction_rev TEXT,
    source_hash TEXT,
    detail TEXT,
    PRIMARY KEY(job_id, entry_id)
);
CREATE INDEX IF NOT EXISTS wiki_transfer_entries_order
    ON wiki_transfer_entries(job_id, ordinal);
";

#[derive(Clone, Debug)]
pub struct JobHead {
    pub job_id: String,
    pub digest: String,
    pub manifest_json: String,
    pub source_id: String,
    pub root: String,
    pub owner_key: String,
    pub state: String,
    /// `fresh` until an implicit-root publication is attempted, then
    /// `unresolved` or `proved`.
    pub root_attempt: String,
}

#[derive(Clone, Debug)]
pub struct EntryRow {
    pub entry_id: String,
    pub ordinal: i64,
    pub path: String,
    pub rel_path: String,
    pub kind: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub if_rev: String,
    pub received: u64,
    pub outcome: String,
    pub rev: Option<String>,
    pub extraction: Option<String>,
    pub extraction_rev: Option<String>,
    pub source_hash: Option<String>,
    pub detail: Option<String>,
}

/// Borrowed fields written onto one persisted entry. Row identity stays with
/// the caller, so recording an outcome does not copy the stored text.
pub(crate) struct EntryOutcome<'a> {
    pub outcome: &'a str,
    pub rev: Option<&'a str>,
    pub extraction: Option<&'a str>,
    pub extraction_rev: Option<&'a str>,
    pub source_hash: Option<&'a str>,
    pub detail: Option<&'a str>,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn db(err: rusqlite::Error) -> Error {
    Error::internal(format!("wiki transfer store: {err}"))
}

/// Stable owner key. The inputs are the daemon caller label and the verified
/// authority tuple; the hex digest does not echo another caller's binding.
pub fn owner_key(actor: &str, authority: Option<&Value>) -> String {
    let body = match authority {
        None => json!({"namespace": "local", "actor": actor}),
        Some(binding) => json!({
            "namespace": "hosted",
            "actor": actor,
            "issuer": binding["issuer"],
            "organization_id": binding["organization_id"],
            "audience": binding["audience"],
            "subject": binding["subject"],
        }),
    };
    let mut hasher = Sha256::new();
    hasher.update(body.to_string().as_bytes());
    format!("wk:{:x}", hasher.finalize())
}

fn map_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<EntryRow> {
    let size: i64 = row.get(5)?;
    let received: i64 = row.get(8)?;
    Ok(EntryRow {
        entry_id: row.get(0)?,
        ordinal: row.get(1)?,
        path: row.get(2)?,
        rel_path: row.get(3)?,
        kind: row.get(4)?,
        size: size.max(0) as u64,
        sha256: row.get(6)?,
        if_rev: row.get(7)?,
        received: received.max(0) as u64,
        outcome: row.get(9)?,
        rev: row.get(10)?,
        extraction: row.get(11)?,
        extraction_rev: row.get(12)?,
        source_hash: row.get(13)?,
        detail: row.get(14)?,
    })
}

const ENTRY_SQL: &str = "SELECT entry_id,ordinal,path,rel_path,kind,size,sha256,if_rev,received,outcome,rev,extraction,extraction_rev,source_hash,detail FROM wiki_transfer_entries";

impl Store {
    pub(crate) fn wiki_transfer_insert(&self, job: &JobHead, entries: &[EntryRow]) -> Result<()> {
        self.write_tx(|conn| {
            conn.execute(
                "INSERT INTO wiki_transfer_jobs(job_id,digest,manifest_json,source_id,root,owner_key,state,root_attempt,index_kicked,created_at)
                 VALUES(?,?,?,?,?,?,?,?,0,?)",
                params![
                    job.job_id,
                    job.digest,
                    job.manifest_json,
                    job.source_id,
                    job.root,
                    job.owner_key,
                    job.state,
                    job.root_attempt,
                    unix_now()
                ],
            )
            .map_err(db)?;
            for entry in entries {
                conn.execute(
                    "INSERT INTO wiki_transfer_entries(job_id,entry_id,ordinal,path,rel_path,kind,size,sha256,if_rev,received,outcome,rev,extraction,extraction_rev,source_hash,detail)
                     VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    params![
                        job.job_id,
                        entry.entry_id,
                        entry.ordinal,
                        entry.path,
                        entry.rel_path,
                        entry.kind,
                        entry.size as i64,
                        entry.sha256,
                        entry.if_rev,
                        0i64,
                        "pending",
                        None::<String>,
                        None::<String>,
                        None::<String>,
                        None::<String>,
                        None::<String>,
                    ],
                )
                .map_err(db)?;
            }
            Ok(())
        })
    }

    pub(crate) fn wiki_transfer_job(&self, job_id: &str) -> Result<Option<JobHead>> {
        self.read_tx(|conn| {
            let row = conn
                .query_opt(
                    "SELECT job_id,digest,manifest_json,source_id,root,owner_key,state,root_attempt FROM wiki_transfer_jobs WHERE job_id=?",
                    [job_id],
                    |row| {
                        Ok(JobHead {
                            job_id: row.get(0)?,
                            digest: row.get(1)?,
                            manifest_json: row.get(2)?,
                            source_id: row.get(3)?,
                            root: row.get(4)?,
                            owner_key: row.get(5)?,
                            state: row.get(6)?,
                            root_attempt: row.get(7)?,
                        })
                    },
                )
                .map_err(db)?;
            Ok(row)
        })
    }

    /// A finish receipt can commit immediately before the daemon loses its
    /// in-memory index wake. Boot re-enqueues one coalesced freshness check.
    pub(crate) fn wiki_transfer_index_recovery_needed(&self) -> Result<bool> {
        self.read_tx(|conn| {
            conn.query_opt(
                "SELECT 1 FROM wiki_transfer_jobs WHERE index_kicked=1 LIMIT 1",
                [],
                |_| Ok(()),
            )
            .map(|job| job.is_some())
            .map_err(db)
        })
    }

    pub(crate) fn wiki_transfer_entries(&self, job_id: &str) -> Result<Vec<EntryRow>> {
        self.read_tx(|conn| {
            conn.query_vec(
                &format!("{ENTRY_SQL} WHERE job_id=? ORDER BY ordinal"),
                [job_id],
                map_entry,
            )
            .map_err(db)
        })
    }

    pub(crate) fn wiki_transfer_entry(
        &self,
        job_id: &str,
        entry_id: &str,
    ) -> Result<Option<EntryRow>> {
        self.read_tx(|conn| {
            conn.query_opt(
                &format!("{ENTRY_SQL} WHERE job_id=? AND entry_id=?"),
                params![job_id, entry_id],
                map_entry,
            )
            .map_err(db)
        })
    }

    /// Advance received bytes only forward. The staged file is already
    /// durable; this row may lag it.
    pub(crate) fn wiki_transfer_note_received(
        &self,
        job_id: &str,
        entry_id: &str,
        received: u64,
    ) -> Result<()> {
        self.write_tx(|conn| {
            conn.execute(
                "UPDATE wiki_transfer_entries SET received=? WHERE job_id=? AND entry_id=? AND received<?",
                params![received as i64, job_id, entry_id, received as i64],
            )
            .map_err(db)?;
            Ok(())
        })
    }

    /// Implicit-root fence. `fresh`, `unresolved`, and `proved` are the only
    /// values transfer writes. This is not an entry outcome and not the
    /// index kick.
    pub(crate) fn wiki_transfer_note_root_attempt(
        &self,
        job_id: &str,
        attempt: &str,
    ) -> Result<()> {
        self.write_tx(|conn| {
            conn.execute(
                "UPDATE wiki_transfer_jobs SET root_attempt=? WHERE job_id=?",
                params![attempt, job_id],
            )
            .map_err(db)?;
            Ok(())
        })
    }

    pub(crate) fn wiki_transfer_record_outcome(
        &self,
        job_id: &str,
        entry_id: &str,
        outcome: &EntryOutcome<'_>,
    ) -> Result<()> {
        self.write_tx(|conn| {
            conn.execute(
                "UPDATE wiki_transfer_entries
                 SET outcome=?, rev=?, extraction=?, extraction_rev=?, source_hash=?, detail=?
                 WHERE job_id=? AND entry_id=?",
                params![
                    outcome.outcome,
                    outcome.rev,
                    outcome.extraction,
                    outcome.extraction_rev,
                    outcome.source_hash,
                    outcome.detail,
                    job_id,
                    entry_id
                ],
            )
            .map_err(db)?;
            Ok(())
        })
    }

    /// Close the job once every entry is terminal. `kick` is true only the
    /// first time a finish observes that close, so a retry does not request
    /// another index rebuild after the flag is durable.
    pub(crate) fn wiki_transfer_finish(
        &self,
        job_id: &str,
        state: &str,
        kick: bool,
    ) -> Result<bool> {
        self.write_tx(|conn| {
            let already: i64 = conn
                .query_row(
                    "SELECT index_kicked FROM wiki_transfer_jobs WHERE job_id=?",
                    [job_id],
                    |row| row.get(0),
                )
                .map_err(db)?;
            let do_kick = kick && already == 0;
            conn.execute(
                "UPDATE wiki_transfer_jobs SET state=?, index_kicked=CASE WHEN ?=1 THEN 1 ELSE index_kicked END WHERE job_id=?",
                params![state, if do_kick { 1 } else { 0 }, job_id],
            )
            .map_err(db)?;
            Ok(do_kick)
        })
    }
}
