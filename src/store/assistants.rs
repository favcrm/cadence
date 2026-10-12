//! CAD-1380/AOS-216: member personal-assistant bindings — the daemon-owned
//! record a verified `(org, sub)` member maps to. One row per
//! `(org, sub)`; `alias` names the daemon-registered assistant agent and
//! `conversation_id` the member's principal-scoped conversation. Rows are
//! written only by daemon-side provisioning (`assist_bind`), never by a
//! request field; the `member_as` relay param re-binds to these rows.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::*;

/// A stored member job request — the provenance a deferred commit
/// re-proves at effect time (SEC-002 / RISK-001).
#[derive(Debug, Clone)]
pub struct AssistRequest {
    pub org: String,
    pub sub: String,
    pub request_key: String,
    pub job: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub acceptance: Option<String>,
    /// The issuing credential the request was minted under — the
    /// revalidation names THIS credential, never a later mint's.
    pub credential_id: Option<String>,
    pub authority_rev: Option<String>,
    /// `requested` | `awaiting_approval` | `dispatched` | `done`.
    pub state: String,
    pub created: f64,
}

/// A stored member-assistant binding.
#[derive(Debug, Clone)]
pub struct AssistantBinding {
    pub org: String,
    pub sub: String,
    pub assistant_id: String,
    pub alias: String,
    pub conversation_id: Option<String>,
    /// `active` | `archived` | `revoked`.
    pub state: String,
    /// The platform authority revision this binding last proved against —
    /// bumped on revalidation so effect-time checks see membership drift.
    pub authority_rev: Option<String>,
    pub created: f64,
    pub updated: f64,
}

/// The pin prefix of a stored `assist_requests.authority_rev` —
/// `"<pin>.<job_updated_ms>"`. The pin is the sequence the reconcile
/// bumps; the suffix is a watermark snapshot, not authority. A legacy
/// or hand-written value that does not parse yields pin `0`.
fn rev_pin(stored: Option<&str>) -> String {
    stored
        .and_then(|s| s.split('.').next().map(str::to_string))
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or_else(|| "0".to_string())
}

/// The next pin — a decimal counter, never reused.
fn pin_next(pin: &str) -> String {
    pin.parse::<u64>().map(|n| n + 1).unwrap_or(1).to_string()
}

/// The job watermark component — whole milliseconds, stable across the
/// float epoch boundary a stringified `f64` would wobble on.
fn rev_millis(updated: f64) -> i64 {
    (updated * 1000.0) as i64
}

fn row_binding(row: &rusqlite::Row) -> rusqlite::Result<AssistantBinding> {
    Ok(AssistantBinding {
        org: row.get("org")?,
        sub: row.get("sub")?,
        assistant_id: row.get("assistant_id")?,
        alias: row.get("alias")?,
        conversation_id: row.get("conversation_id")?,
        state: row.get("state")?,
        authority_rev: row.get("authority_rev")?,
        created: row.get("created")?,
        updated: row.get("updated")?,
    })
}

impl AssistantBinding {
    /// Live iff `active` — `archived`/`revoked` bindings refuse effects but
    /// keep history.
    pub fn is_active(&self) -> bool {
        self.state == "active"
    }

    pub fn to_json(&self) -> Value {
        json!({
            "org": self.org,
            "sub": self.sub,
            "assistant_id": self.assistant_id,
            "alias": self.alias,
            "conversation_id": self.conversation_id,
            "state": self.state,
            "authority_rev": self.authority_rev,
            "created": self.created,
            "updated": self.updated,
        })
    }
}

impl Store {
    /// The binding for `(org, sub)`, or `None`.
    pub fn assistant_binding(&self, org: &str, sub: &str) -> Result<Option<AssistantBinding>> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT * FROM assistants WHERE org=? AND sub=?",
            params![org, sub],
            row_binding,
        ) {
            Ok(b) => Ok(Some(b)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The binding for `(org, assistant_id)`, or `None` — used to refuse a
    /// member reaching another principal's assistant.
    pub fn assistant_by_id(
        &self,
        org: &str,
        assistant_id: &str,
    ) -> Result<Option<AssistantBinding>> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT * FROM assistants WHERE org=? AND assistant_id=?",
            params![org, assistant_id],
            row_binding,
        ) {
            Ok(b) => Ok(Some(b)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The binding by registered agent `alias` — the daemon's source-
    /// verified "is this alias a bound member assistant" check for the
    /// agent-class gate (`member_assist_policy`). One row, or `None`.
    pub fn assistant_for_alias(&self, alias: &str) -> Result<Option<AssistantBinding>> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT * FROM assistants WHERE alias=?",
            params![alias],
            row_binding,
        ) {
            Ok(b) => Ok(Some(b)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Provision or refresh the binding. Daemon-only (the `member_as` proof
    /// gates the caller). Idempotent per `(org, sub)`: re-provisioning a live
    /// binding returns the existing row; a `revoked`/`archived` one returns
    /// `Err` rather than silently reviving stale authority.
    pub fn assistant_bind(
        &self,
        org: &str,
        sub: &str,
        assistant_id: &str,
        alias: &str,
        authority_rev: Option<&str>,
    ) -> Result<AssistantBinding> {
        let now = crate::issue::time::now_epoch() as f64;
        self.write_tx(|tx| {
            if let Ok(existing) = tx.query_row(
                "SELECT * FROM assistants WHERE org=? AND sub=?",
                params![org, sub],
                row_binding,
            ) {
                if !existing.is_active() {
                    return Err(Error::rejected(format!(
                        "assistant binding for this member is {}; it is not \
                         revived by a request — re-provision through the platform",
                        existing.state
                    )));
                }
                // Refresh the proven authority revision; idempotent.
                tx.execute(
                    "UPDATE assistants SET authority_rev=COALESCE(?,authority_rev), \
                     updated=? WHERE org=? AND sub=?",
                    params![authority_rev, now, org, sub],
                )?;
                return Ok(existing);
            }
            tx.execute(
                "INSERT INTO assistants(org, sub, assistant_id, alias, state, \
                 authority_rev, created, updated) VALUES(?,?,?,?,'active',?,?,?)",
                params![org, sub, assistant_id, alias, authority_rev, now, now],
            )?;
            let binding = tx.query_row(
                "SELECT * FROM assistants WHERE org=? AND sub=?",
                params![org, sub],
                row_binding,
            )?;
            Ok(binding)
        })
    }

    /// Set the binding's conversation once the daemon created it.
    pub fn assistant_set_conversation(
        &self,
        org: &str,
        sub: &str,
        conversation_id: &str,
    ) -> Result<()> {
        let now = crate::issue::time::now_epoch() as f64;
        self.write_tx(|tx| {
            tx.execute(
                "UPDATE assistants SET conversation_id=?, updated=? WHERE org=? AND sub=?",
                params![conversation_id, now, org, sub],
            )?;
            Ok(())
        })
    }

    /// Record a member's authority revocation — the row stays for history
    /// but `is_active` goes false so every member effect refuses.
    pub fn assistant_revoke(&self, org: &str, sub: &str) -> Result<bool> {
        let now = crate::issue::time::now_epoch() as f64;
        self.write_tx(|tx| {
            let n = tx.execute(
                "UPDATE assistants SET state='revoked', updated=? WHERE org=? AND sub=? \
                 AND state='active'",
                params![now, org, sub],
            )?;
            Ok(n > 0)
        })
    }

    /// The (org, sub) member's stored authority revision for effect-time
    /// revalidation — `Err`/None refuses when unproven.
    pub fn assistant_authority_rev(&self, org: &str, sub: &str) -> Result<Option<String>> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT authority_rev FROM assistants WHERE org=? AND sub=? AND state='active'",
            params![org, sub],
            |r| r.get(0),
        ) {
            Ok(v) => Ok(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ---- shared-job subscribers ---------------------------------------

    /// Subscribe `(org, sub)` to `job` through their assistant — idempotent.
    pub fn job_subscribe(&self, job: &str, org: &str, sub: &str, assistant_id: &str) -> Result<()> {
        let now = crate::issue::time::now_epoch() as f64;
        self.write_tx(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO job_subscribers(job, org, sub, assistant_id, created) \
                 VALUES(?,?,?,?,?)",
                params![job, org, sub, assistant_id, now],
            )?;
            Ok(())
        })
    }

    /// Is `(org, sub)` a subscriber (or may they see this job) — the read ACL.
    pub fn job_is_subscriber(&self, job: &str, org: &str, sub: &str) -> Result<bool> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT 1 FROM job_subscribers WHERE job=? AND org=? AND sub=?",
            params![job, org, sub],
            |_| Ok(()),
        ) {
            Ok(()) => Ok(true),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Subscribers of `job` — for delivery fan-out (metadata only).
    pub fn job_subscribers(&self, job: &str) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT org, sub, assistant_id FROM job_subscribers WHERE job=? ORDER BY created",
        )?;
        let rows = stmt
            .query_map([job], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- owner-granted job sharing (the subscription entitlement) ------

    /// The owning member enables (or disables) sharing of their own
    /// assist-bound job inside their org — the ONLY way a job becomes
    /// subscribable by a second member. `enabled=false` clears the row;
    /// existing subscriber rows are not retro-removed (each still
    /// re-passes the live member re-prove on every read).
    pub fn assist_job_share(
        &self,
        job: &str,
        org: &str,
        shared_by: &str,
        enabled: bool,
    ) -> Result<()> {
        let now = crate::issue::time::now_epoch() as f64;
        self.write_tx(|tx| {
            if enabled {
                tx.execute(
                    "INSERT OR REPLACE INTO assist_job_shares(job, org, shared_by, created) \
                     VALUES(?,?,?,?)",
                    params![job, org, shared_by, now],
                )?;
            } else {
                tx.execute(
                    "DELETE FROM assist_job_shares WHERE job=? AND org=?",
                    params![job, org],
                )?;
            }
            Ok(())
        })
    }

    /// The member-visible authority revision of an assist-bound job —
    /// `"{pin}.{job_updated_ms}"`. `pin` is the monotonically increasing
    /// counter stored in `assist_requests.authority_rev`'s prefix
    /// (bumped by every reconcile); `job_updated_ms` is the job's
    /// CURRENT `updated` watermark in whole milliseconds, re-read live —
    /// so a revision a member observed before the job's queued effect
    /// moved (a new task, a dispatch, a state change) is stale and
    /// refuses, and only the exact current revision admits.
    /// `None` when the job is not assist-bound or has no job row.
    pub fn assist_job_authority_rev(&self, job: &str) -> Result<Option<String>> {
        let conn = self.conn();
        let stored: Option<Option<String>> = conn
            .query_row(
                "SELECT authority_rev FROM assist_requests WHERE job=? ORDER BY created LIMIT 1",
                [job],
                |r| r.get(0),
            )
            .optional()?;
        let Some(stored) = stored else {
            return Ok(None);
        };
        let updated: Option<f64> = conn
            .query_row("SELECT updated FROM jobs WHERE id=?", [job], |r| r.get(0))
            .optional()?;
        let Some(updated) = updated else {
            return Ok(None);
        };
        let pin = rev_pin(stored.as_deref());
        Ok(Some(format!("{pin}.{}", rev_millis(updated))))
    }

    /// The owning member's explicit job-specific reconciliation: re-pin
    /// a deferred assist job's recorded `assist_requests.credential_id`
    /// to the member's CURRENT live credential, bound to the exact
    /// `expected_authority_rev` the member named — the revision the
    /// queued op committed under, pinned to the canonical (org, sub,
    /// job) request AND the job's current `updated` watermark. The
    /// compare and the write run in ONE transaction: a stale/wrong rev,
    /// a non-owner tuple, or a missing row refuses with NO change
    /// (no TOCTOU, no blind re-pin, no silent adoption of a moved or
    /// replayed authority). Returns the NEW stored revision.
    pub fn assist_request_reconcile(
        &self,
        org: &str,
        sub: &str,
        job: &str,
        credential_id: &str,
        expected_authority_rev: &str,
    ) -> Result<String> {
        self.write_tx(|tx| {
            let row: Option<Option<String>> = tx
                .query_row(
                    "SELECT authority_rev FROM assist_requests \
                     WHERE org=? AND sub=? AND job=?",
                    params![org, sub, job],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(stored) = row else {
                return Err(Error::rejected(
                    "assist request reconcile refused: no such assist-bound job",
                ));
            };
            let updated: Option<f64> = tx
                .query_row("SELECT updated FROM jobs WHERE id=?", [job], |r| r.get(0))
                .optional()?;
            let Some(updated) = updated else {
                return Err(Error::rejected(
                    "assist request reconcile refused: the job record is unverifiable",
                ));
            };
            let pin = rev_pin(stored.as_deref());
            let current = format!("{pin}.{}", rev_millis(updated));
            if expected_authority_rev != current {
                return Err(Error::rejected(
                    "assist request reconcile refused: authority rev mismatch — the \
                     expected rev does not name the job's current committed revision",
                ));
            }
            let next = format!("{}.{}", pin_next(&pin), rev_millis(updated));
            tx.execute(
                "UPDATE assist_requests SET credential_id=?, authority_rev=? \
                 WHERE org=? AND sub=? AND job=?",
                params![credential_id, next, org, sub, job],
            )?;
            Ok(next)
        })
    }

    /// Is `job` shared inside `org` — the entitlement `member_job_subscribe`
    /// requires before a second member's `job_subscribers` row may exist.
    pub fn assist_job_is_shared(&self, job: &str, org: &str) -> Result<bool> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT 1 FROM assist_job_shares WHERE job=? AND org=?",
            params![job, org],
            |_| Ok(()),
        ) {
            Ok(()) => Ok(true),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// The job ids `(org, sub)` requested or is subscribed to — the union of
    /// the request ledger and the subscriber set, for `member_job_list`.
    pub fn assist_member_jobs(&self, org: &str, sub: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT job FROM assist_requests WHERE org=? AND sub=? \
             UNION SELECT job FROM job_subscribers WHERE org=? AND sub=?",
        )?;
        let rows = stmt
            .query_map(params![org, sub, org, sub], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    /// Did `(org, sub)` create `job` through a recorded request — the owner
    /// side of the read ACL (with `job_is_subscriber`).
    pub fn assist_request_owns(&self, org: &str, sub: &str, job: &str) -> Result<bool> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT 1 FROM assist_requests WHERE job=? AND org=? AND sub=? LIMIT 1")?;
        stmt.exists(params![job, org, sub]).map_err(Into::into)
    }

    // ---- idempotent job request ledger --------------------------------

    /// The canonical job a `(org, sub, request_key)` maps to, or `None`.
    pub fn assist_request_job(
        &self,
        org: &str,
        sub: &str,
        request_key: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT job FROM assist_requests WHERE org=? AND sub=? AND request_key=?",
            params![org, sub, request_key],
            |r| r.get(0),
        ) {
            Ok(job) => Ok(Some(job)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Bind `(org, sub, request_key)` → `job` — the request's canonical
    /// identity, persisted WITH its title/body/acceptance and the
    /// authorization provenance (issuing credential + the pinned
    /// authority revision `1.<job_updated_ms>`) a deferred commit
    /// re-proves and a `member_job_reconcile` must name. `INSERT OR
    /// IGNORE` so a concurrent or replayed request returns the existing
    /// mapping (read back) instead of double-creating; the deferred
    /// consumer never adopts a later mint's credential.
    #[allow(clippy::too_many_arguments)]
    pub fn assist_request_bind(
        &self,
        org: &str,
        sub: &str,
        request_key: &str,
        job: &str,
        title: &str,
        body: &str,
        acceptance: &str,
        credential_id: &str,
    ) -> Result<String> {
        let now = crate::issue::time::now_epoch() as f64;
        self.write_tx(|tx| {
            // The initial pinned revision binds the request to the job's
            // `updated` watermark at bind — `1.<ms>`. A moved job bumps
            // the member-visible rev (`assist_job_authority_rev`), so a
            // reconcile must pin the CURRENT one.
            let updated: Option<f64> = tx
                .query_row("SELECT updated FROM jobs WHERE id=?", [job], |r| r.get(0))
                .optional()?;
            let authority_rev = format!("1.{}", rev_millis(updated.unwrap_or(0.0)));
            tx.execute(
                "INSERT OR IGNORE INTO assist_requests(
                    org, sub, request_key, job, title, body, acceptance,
                    credential_id, authority_rev, state, created)
                 VALUES(?,?,?,?,?,?,?,?,?,'requested',?)",
                params![
                    org,
                    sub,
                    request_key,
                    job,
                    title,
                    body,
                    acceptance,
                    credential_id,
                    authority_rev,
                    now
                ],
            )?;
            // The committed winner — this request's canonical job.
            let winner = tx.query_row(
                "SELECT job FROM assist_requests WHERE org=? AND sub=? AND request_key=?",
                params![org, sub, request_key],
                |r| r.get(0),
            )?;
            Ok(winner)
        })
    }

    /// The stored request a deferred commit re-proves — its recorded
    /// provenance (issuing credential + authority revision) is what the
    /// revalidation names, never a caller-supplied value.
    pub fn assist_request_record(
        &self,
        org: &str,
        sub: &str,
        request_key: &str,
    ) -> Result<Option<AssistRequest>> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT org, sub, request_key, job, title, body, acceptance,
                    credential_id, authority_rev, state, created
             FROM assist_requests WHERE org=? AND sub=? AND request_key=?",
            params![org, sub, request_key],
            |r| {
                Ok(AssistRequest {
                    org: r.get("org")?,
                    sub: r.get("sub")?,
                    request_key: r.get("request_key")?,
                    job: r.get("job")?,
                    title: r.get("title")?,
                    body: r.get("body")?,
                    acceptance: r.get("acceptance")?,
                    credential_id: r.get("credential_id")?,
                    authority_rev: r.get("authority_rev")?,
                    state: r.get("state")?,
                    created: r.get("created")?,
                })
            },
        ) {
            Ok(rec) => Ok(Some(rec)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The request record bound to `job` (the deferred-commit lookup by
    /// job id, for the commit-time revalidation). `None` when the job was
    /// not created through the member request path.
    pub fn assist_request_for_job(&self, job: &str) -> Result<Option<AssistRequest>> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT org, sub, request_key, job, title, body, acceptance,
                    credential_id, authority_rev, state, created
             FROM assist_requests WHERE job=? ORDER BY created LIMIT 1",
            [job],
            |r| {
                Ok(AssistRequest {
                    org: r.get("org")?,
                    sub: r.get("sub")?,
                    request_key: r.get("request_key")?,
                    job: r.get("job")?,
                    title: r.get("title")?,
                    body: r.get("body")?,
                    acceptance: r.get("acceptance")?,
                    credential_id: r.get("credential_id")?,
                    authority_rev: r.get("authority_rev")?,
                    state: r.get("state")?,
                    created: r.get("created")?,
                })
            },
        ) {
            Ok(rec) => Ok(Some(rec)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ---- deduplicated stall checks ------------------------------------

    /// At most one OPEN stall check per (job, revision): `INSERT OR
    /// IGNORE` — the check exists or is created. `Ok(true)` when this
    /// call created the open check (the supervisor should act on it);
    /// `Ok(false)` when an unresolved check already stands (dedup).
    pub fn assist_stall_mark(&self, job: &str, revision: i64) -> Result<bool> {
        let now = crate::issue::time::now_epoch() as f64;
        self.write_tx(|tx| {
            // Resolve any prior (older-revision) open checks for this job;
            // a check is bound to the revision it observed.
            tx.execute(
                "UPDATE assist_stall_checks SET state='resolved', resolved=? \
                 WHERE job=? AND revision<>? AND state='open'",
                params![now, job, revision],
            )?;
            let n = tx.execute(
                "INSERT OR IGNORE INTO assist_stall_checks(job, revision, state, created) \
                 VALUES(?,?,'open',?)",
                params![job, revision, now],
            )?;
            Ok(n > 0)
        })
    }

    /// Resolve the open check for (job, revision) — a progress watermark
    /// or a manual intervention closed it.
    pub fn assist_stall_resolve(&self, job: &str, revision: i64) -> Result<()> {
        let now = crate::issue::time::now_epoch() as f64;
        self.write_tx(|tx| {
            tx.execute(
                "UPDATE assist_stall_checks SET state='resolved', resolved=? \
                 WHERE job=? AND revision=? AND state='open'",
                params![now, job, revision],
            )?;
            Ok(())
        })
    }

    /// Jobs an assistant-requested lane is still responsible for —
    /// the supervisor scans these (non-terminal job, member-requested).
    pub fn assist_active_jobs(&self) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT r.job, r.org, r.sub FROM assist_requests r \
             JOIN jobs j ON j.id = r.job \
             WHERE j.state NOT IN ('done','cancelled','failed')",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// One supervisor sweep over `assist_active_jobs`: for every
    /// non-terminal assist-bound job whose progress watermark (the
    /// job's own `updated`, which every task transition touches) is
    /// older than `stall_secs` — the job's budget, else the daemon
    /// default the PM checkup uses — one `assist_stall` event on the
    /// job's stream, dedup'd per watermark by `assist_stall_mark`.
    /// Revision keys the dedup: a job that moves resets the check, so
    /// the same stall never re-fires while a NEW stall does. Never
    /// dispatches, never widens — read + mark + one event.
    pub fn assist_stall_sweep(&self, default_stall_secs: i64, now: f64) -> Result<u32> {
        let jobs = self.assist_active_jobs()?;
        let mut stalled = 0u32;
        for (job_id, _org, _sub) in jobs {
            // The watermark is the job's `updated` — advanced by every
            // task/verdict/accept that touches the job. Read the job
            // for its bound + watermark in one tx, then mark + event.
            let flagged = self.write_tx(|tx| {
                let row = tx.query_row(
                    "SELECT updated, stall_secs, pm_alias, state FROM jobs WHERE id=?",
                    [job_id.as_str()],
                    |r| {
                        Ok((
                            r.get::<_, f64>(0)?,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                        ))
                    },
                );
                let (updated, stall_secs, pm_alias, state) = match row {
                    Ok(v) => v,
                    Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(false),
                    Err(e) => return Err(e.into()),
                };
                if matches!(state.as_str(), "done" | "cancelled" | "failed") {
                    return Ok(false);
                }
                let budget = stall_secs.unwrap_or(default_stall_secs).max(60);
                if now - updated < budget as f64 {
                    return Ok(false);
                }
                let revision = updated as i64;
                // Resolve older-revision checks; open one for this
                // watermark (INSERT OR IGNORE dedups a re-fire).
                tx.execute(
                    "UPDATE assist_stall_checks SET state='resolved', resolved=? \
                     WHERE job=? AND revision<>? AND state='open'",
                    params![now, job_id.as_str(), revision],
                )?;
                let n = tx.execute(
                    "INSERT OR IGNORE INTO assist_stall_checks(job, revision, state, created) \
                     VALUES(?,?,'open',?)",
                    params![job_id.as_str(), revision, now],
                )?;
                if n == 0 {
                    return Ok(false); // already flagged at this watermark
                }
                Self::event_scoped(
                    tx,
                    &pm_alias,
                    "assist_stall",
                    json!({
                        "job": job_id,
                        "revision": revision,
                        "stalled_secs": (now - updated) as i64,
                        "budget_secs": budget,
                        "by": "assist-supervisor",
                    }),
                    Some(&job_id),
                    None,
                )?;
                // Route the escalation to every authorized subscriber's
                // OWN assistant alias — the member-facing feed is the
                // subscriber's private conversation stream, never the
                // owner's PM lane alone. The owner's (org,sub) is the
                // `assist_requests` row that minted the job; a
                // subscriber whose binding is not active holds no live
                // alias to escalate to and is skipped.
                let owner: Option<(String, String)> = tx
                    .query_row(
                        "SELECT org, sub FROM assist_requests WHERE job=? LIMIT 1",
                        [job_id.as_str()],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let subs: Vec<(String, String)> = tx.query_vec(
                    "SELECT org, sub FROM job_subscribers WHERE job=?",
                    [job_id.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                for (sorg, ssub) in subs {
                    if owner
                        .as_ref()
                        .is_some_and(|(o, s)| *o == sorg && *s == ssub)
                    {
                        continue; // the owner is pm_alias — already written
                    }
                    let alias: Option<String> = tx
                        .query_row(
                            "SELECT alias FROM assistants WHERE org=? AND sub=? AND state='active'",
                            params![sorg, ssub],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if let Some(sub_alias) = alias {
                        Self::event_scoped(
                            tx,
                            &sub_alias,
                            "assist_stall",
                            json!({
                                "job": job_id,
                                "revision": revision,
                                "stalled_secs": (now - updated) as i64,
                                "budget_secs": budget,
                                "by": "assist-supervisor",
                            }),
                            Some(&job_id),
                            None,
                        )?;
                    }
                }
                Ok(true)
            })?;
            // `Ok(true)` is the tx's report that THIS job newly flagged —
            // the sweep counts only newly-marked checks, so a dedup'd
            // re-fire and an in-budget job never count.
            if flagged {
                stalled += 1;
            }
        }
        Ok(stalled)
    }
}
