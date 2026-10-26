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
    /// `pending` | `complete` | `skipped` | `deferred` (CAD-1420).
    pub bootstrap_state: String,
    pub bootstrap_template_version: i64,
    /// Compare-and-set counter: every accepted bootstrap write bumps it.
    pub bootstrap_revision: i64,
}

/// The lifecycle/profile mutation committed under one bootstrap revision fence.
pub struct AssistantBootstrapUpdate<'a> {
    pub action: &'a str,
    pub expected_revision: i64,
    pub look: Option<&'a str>,
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
        bootstrap_state: row.get("bootstrap_state")?,
        bootstrap_template_version: row.get("bootstrap_template_version")?,
        bootstrap_revision: row.get("bootstrap_revision")?,
    })
}

/// The local provenance a preflight captured for an assist-bound dispatch.
/// Carried into the fenced transaction so a revocation, removal or re-mint
/// between preflight and commit refuses, never silently kicks off.
#[derive(Debug, Clone)]
pub struct AssistDispatchExpect {
    pub org: String,
    pub sub: String,
    pub assistant_id: String,
    pub alias: String,
    /// The ORIGINAL ledger credential (`assist_requests.credential_id`).
    /// Never borrowed from the current binding.
    pub credential_id: Option<String>,
    pub job: String,
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
            "bootstrap_state": self.bootstrap_state,
            "bootstrap_template_version": self.bootstrap_template_version,
            "bootstrap_revision": self.bootstrap_revision,
        })
    }
}

impl Store {
    /// Re-read the ledger row and binding inside the caller's fenced
    /// transaction. Every field of the captured tuple must still match and the
    /// binding must be active; otherwise refuse without writing.
    pub(super) fn assist_expect_in(
        &self,
        conn: &impl StoreConn,
        expect: &AssistDispatchExpect,
    ) -> Result<()> {
        let refused = |why: &str| {
            Error::rejected(format!(
                "task dispatch refused: the assist-bound job's {why} changed or was revoked \
                 between preflight and commit (CAD-1380)"
            ))
        };
        let ledger = conn
            .query_opt(
                "SELECT org, sub, credential_id FROM assist_requests WHERE job=? \
                 ORDER BY created LIMIT 1",
                [&expect.job],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )?
            .ok_or_else(|| refused("ledger row"))?;
        if ledger.0 != expect.org || ledger.1 != expect.sub || ledger.2 != expect.credential_id {
            return Err(refused("ledger provenance"));
        }
        let binding = conn
            .query_opt(
                "SELECT * FROM assistants WHERE org=? AND sub=?",
                params![expect.org, expect.sub],
                row_binding,
            )?
            .ok_or_else(|| refused("binding"))?;
        if !binding.is_active()
            || binding.assistant_id != expect.assistant_id
            || binding.alias != expect.alias
        {
            return Err(refused("binding"));
        }
        Ok(())
    }

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
                // CAD-1420 CAS: the stored tuple IS the member's canonical
                // identity — a caller naming a different assistant_id or
                // alias for the same (org, sub) is a conflict the
                // transaction refuses, never a silent adoption of a
                // divergent binding.
                if existing.assistant_id != assistant_id || existing.alias != alias {
                    return Err(Error::rejected(
                        "assistant binding for this member already exists under a \
                         different assistant/alias — a conflicting tuple is refused, \
                         never rewritten",
                    ));
                }
                // Refresh the proven authority revision; idempotent.
                tx.execute(
                    "UPDATE assistants SET authority_rev=COALESCE(?,authority_rev), \
                     updated=? WHERE org=? AND sub=?",
                    params![authority_rev, now, org, sub],
                )?;
                return Ok(existing);
            }
            // CAD-1420: a NEW binding's daemon-derived alias must not collide
            // with a row already registered as an ordinary agent — checked in
            // this same transaction so a racing registration or a squatting
            // worker is refused, never adopted as the member's assistant.
            let alias_taken: bool = tx
                .query_opt("SELECT 1 FROM agents WHERE alias=?", [alias], |_| Ok(()))?
                .is_some();
            if alias_taken {
                return Err(Error::rejected(format!(
                    "assistant alias '{alias}' is already registered — the binding \
                     refuses rather than adopt an ordinary agent"
                )));
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

    /// CAD-1420: apply one bootstrap lifecycle transition as a compare-and-set
    /// inside one fenced write. Every guard reads the rows INSIDE the same
    /// transaction: the message still `running` under `turn_token` for this
    /// alias, its immutable member stamp equal to `expect` (org, sub,
    /// assistant, original credential), the generation-current agent, the
    /// binding active for the same tuple, and `bootstrap_revision` equal to
    /// `expected_revision`. Only then does `apply_files` run (local profile
    /// files, no network), the owning agent's `params.look` is pinned when
    /// `look` is given, and only after both succeed is the state written.
    /// Unknown actions or states refuse before any effect. Any refusal writes
    /// nothing and returns the error unchanged.
    pub fn assistant_bootstrap_apply(
        &self,
        expect: &AssistDispatchExpect,
        message_id: &str,
        turn_token: &str,
        update: AssistantBootstrapUpdate<'_>,
        apply_files: impl FnOnce() -> Result<()>,
    ) -> Result<AssistantBinding> {
        let AssistantBootstrapUpdate {
            action,
            expected_revision,
            look,
        } = update;
        let now = crate::issue::time::now_epoch() as f64;
        self.write_tx(|tx| {
            let refused = |why: &str| Error::rejected(format!("bootstrap refused: {why}"));
            let (state, msg_alias, turn): (String, String, Option<String>) = tx
                .query_row(
                    "SELECT state, alias, turn_id FROM messages WHERE id=?",
                    [message_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(|_| refused("the message is unknown"))?;
            if state != "running"
                || turn.as_deref() != Some(turn_token)
                || msg_alias != expect.alias
            {
                return Err(refused("the message is no longer the running turn"));
            }
            let stamp: (String, String, String, String) = tx
                .query_row(
                    "SELECT e.payload FROM thread_entries e \
                     JOIN threads t ON t.id=e.thread_id \
                     JOIN messages m ON m.id=e.message_id AND m.alias=t.alias \
                     WHERE e.message_id=? AND e.role='member' AND e.kind='message' \
                     ORDER BY e.seq LIMIT 1",
                    [message_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
                .and_then(|p| serde_json::from_str::<Value>(&p).ok())
                .and_then(|v| v.get("member_authority").cloned())
                .and_then(|a| {
                    let f = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string);
                    Some((
                        f("org")?,
                        f("sub")?,
                        f("assistant_id")?,
                        f("credential_id")?,
                    ))
                })
                .ok_or_else(|| refused("the message has no immutable member stamp"))?;
            if stamp.0 != expect.org
                || stamp.1 != expect.sub
                || stamp.2 != expect.assistant_id
                || Some(stamp.3.as_str()) != expect.credential_id.as_deref()
            {
                return Err(refused("the member stamp does not match this assistant"));
            }
            // The live runtime row, read in this same fenced transaction: the
            // token must still be current for the endpoint's own provider,
            // kind and generation, so a restarted or re-minted endpoint
            // refuses before any profile effect.
            let (provider, kind, generation): (String, String, Option<String>) = tx
                .query_row(
                    "SELECT provider, endpoint_kind, generation FROM agents WHERE alias=?",
                    [&expect.alias],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(|_| refused("the assistant runtime is not registered"))?;
            if !crate::adapter::registry::turn_token_current(
                &provider,
                &kind,
                generation.as_deref(),
                turn_token,
            ) {
                return Err(refused("the turn token is not current for this endpoint"));
            }
            let binding = tx
                .query_opt(
                    "SELECT * FROM assistants WHERE org=? AND sub=?",
                    params![expect.org, expect.sub],
                    row_binding,
                )?
                .ok_or_else(|| refused("no assistant binding"))?;
            if !binding.is_active()
                || binding.assistant_id != expect.assistant_id
                || binding.alias != expect.alias
            {
                return Err(refused("the assistant binding changed or is not active"));
            }
            if binding.bootstrap_revision != expected_revision {
                return Err(refused(&format!(
                    "stale revision (stored {}, expected {expected_revision})",
                    binding.bootstrap_revision
                )));
            }
            if !matches!(
                binding.bootstrap_state.as_str(),
                "pending" | "complete" | "skipped" | "deferred"
            ) || binding.bootstrap_template_version != 1
            {
                return Err(refused("the stored bootstrap state is unknown"));
            }
            let next_state = match (action, binding.bootstrap_state.as_str()) {
                ("update", state) => state,
                ("complete", "pending") => "complete",
                ("skip", "pending") => "skipped",
                ("defer", "pending") => "deferred",
                ("resume", "deferred") => "pending",
                _ => {
                    return Err(refused(&format!(
                        "'{action}' is not allowed from state '{}'",
                        binding.bootstrap_state
                    )))
                }
            }
            .to_string();
            // REQ-011: an explicit avatar pick is the owning assistant's
            // ordinary `params.look`, written in this same fenced transaction
            // so the projection, the pin and the committed revision move
            // together. No second look store exists.
            let stored_params = if let Some(look) = look {
                let stored: Option<String> = tx
                    .query_row(
                        "SELECT params FROM agents WHERE alias=?",
                        [&expect.alias],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .flatten();
                let mut params = stored
                    .as_deref()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .unwrap_or_else(|| json!({}));
                let object = params
                    .as_object_mut()
                    .ok_or_else(|| refused("the assistant runtime's params are not an object"))?;
                object.insert("look".to_string(), json!(look));
                let stored = params.to_string();
                if stored.len() > 4_000 {
                    return Err(refused("the avatar look does not fit the params cap"));
                }
                Some(stored)
            } else {
                None
            };
            // An invalid avatar merge must refuse before any profile file changes.
            apply_files()?;
            if let Some(stored) = stored_params {
                tx.execute(
                    "UPDATE agents SET params=?, updated=? WHERE alias=?",
                    params![stored, now, expect.alias],
                )?;
            }
            tx.execute(
                "UPDATE assistants SET bootstrap_state=?, \
                 bootstrap_revision=bootstrap_revision+1, updated=? \
                 WHERE org=? AND sub=? AND bootstrap_revision=?",
                params![next_state, now, expect.org, expect.sub, expected_revision],
            )?;
            tx.query_row(
                "SELECT * FROM assistants WHERE org=? AND sub=?",
                params![expect.org, expect.sub],
                row_binding,
            )
            .map_err(Into::into)
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

    /// CAD-1380 R4: the inert `awaiting_approval` notice for a fresh member
    /// request. Runs after the canonical ledger bind and the requester's
    /// auto-subscription are committed. Subject is the job's real initial task
    /// (`create_job` always creates one); with no task it writes nothing.
    /// Idempotent: the intent key is the same digest a replay recomputes, so a
    /// retry writes no duplicate. Writes intents only; never approves or
    /// dispatches, and never changes the spec, request provenance or CID.
    pub fn assist_request_notice(&self, job: &str) -> Result<()> {
        self.write_tx(|tx| {
            let initial: Option<String> = tx
                .query_row(
                    "SELECT id FROM tasks WHERE job_id=? ORDER BY created LIMIT 1",
                    [job],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(task_id) = initial else {
                return Ok(());
            };
            let job_row = self.job_in(tx, job)?;
            let task = self.task_in(tx, &task_id)?;
            self.assist_intents_for_transition(
                tx,
                &job_row,
                &task,
                "awaiting_approval",
                "request",
            )?;
            Ok(())
        })
    }

    /// CAD-1380 R4: the creation time of the kickoff (`dispatch` or
    /// `job_dispatch`) that delivered `task` to `alias`. `None` when no such
    /// delivery exists, so a missing birth proof fails closed.
    pub fn task_kickoff_created(&self, task: &str, alias: &str) -> Result<Option<f64>> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT created FROM messages WHERE task_id=? AND alias=? \
             AND source IN ('dispatch','job_dispatch') ORDER BY created LIMIT 1",
            params![task, alias],
            |r| r.get(0),
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// CAD-1380 R4: the final commit for one assist intent. Runs in ONE
    /// short store transaction with NO HTTP inside it. It reloads the pending
    /// intent, requires its immutable tuple (`job`, `task`, `key`) to match the
    /// caller's proof-selected recipient, rechecks the active binding and the
    /// owner-org/owner-or-subscriber ACL, then enqueues through the existing
    /// `enqueue_tx` and sets the intent's terminal status in the same commit.
    /// Returns `Ok(Some(alias))` when a new notice was queued, `Ok(None)` when
    /// the local authority no longer holds (intent finished as `refused`), and
    /// `Ok(None)` for an identical replay that already exists (no new count).
    /// `Err` only for an unexpected store failure; nothing is then finished.
    pub fn commit_assist_intent(
        &self,
        seq: i64,
        recipient: &AssistRecipient,
        body: &str,
        message_id: &str,
        source: &str,
        task_id: &str,
    ) -> Result<Option<String>> {
        self.write_tx(|tx| {
            let payload: String = tx.query_row(
                "SELECT payload FROM events WHERE seq=? AND kind='assist_intent' \
                 AND json_extract(payload,'$.status')='pending'",
                [seq],
                |r| r.get(0),
            )?;
            let p: Value = serde_json::from_str(&payload)
                .map_err(|_| Error::internal("assist intent payload unreadable"))?;
            let field = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default();
            // The stored intent must still name this exact subscriber tuple,
            // job and task. Any drift is refused, never rerouted.
            if field("target") != "subscriber"
                || field("job") != recipient.job
                || field("task") != task_id
                || field("org") != recipient.org
                || field("sub") != recipient.sub
                || field("assistant_id") != recipient.assistant_id
            {
                return Err(Error::rejected("assist intent tuple changed"));
            }
            let owner_org: Option<String> = tx
                .query_row(
                    "SELECT org FROM assist_requests WHERE job=? LIMIT 1",
                    [recipient.job.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            let binding: Option<(String, String, String, String)> = tx
                .query_row(
                    "SELECT alias,org,sub,state FROM assistants WHERE org=? AND assistant_id=?",
                    params![recipient.org, recipient.assistant_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let member_ok = tx
                .query_row(
                    "SELECT 1 FROM job_subscribers WHERE job=? AND org=? AND sub=?",
                    params![recipient.job, recipient.org, recipient.sub],
                    |_| Ok(()),
                )
                .optional()?
                .is_some()
                || tx
                    .query_row(
                        "SELECT 1 FROM assist_requests WHERE job=? AND org=? AND sub=?",
                        params![recipient.job, recipient.org, recipient.sub],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
            // The binding must still be the alias the proof and confinement
            // check selected: a replacement alias is refused, never routed.
            let authorized = owner_org.as_deref() == Some(recipient.org.as_str())
                && member_ok
                && binding.as_ref().is_some_and(|(alias, org, sub, state)| {
                    alias == &recipient.alias
                        && org == &recipient.org
                        && sub == &recipient.sub
                        && state == "active"
                });
            if !authorized {
                tx.execute(
                    "UPDATE events SET payload=json_set(payload,'$.status','refused') WHERE seq=?",
                    [seq],
                )?;
                return Ok(None);
            }
            let alias = recipient.alias.clone();
            let (duplicate, _) = self.enqueue_tx(
                tx,
                &alias,
                body,
                None,
                message_id,
                source,
                Some(task_id),
                None,
                None,
                &Sender::Unattributed,
                Priority::default(),
                None,
                None,
                None,
            )?;
            tx.execute(
                "UPDATE events SET payload=json_set(payload,'$.status','delivered') WHERE seq=?",
                [seq],
            )?;
            Ok((!duplicate).then_some(alias))
        })
    }

    /// CAD-1380 R4: pending assist-job intents, oldest first, bounded. Each
    /// entry is the intent `events.seq` and its metadata payload. Reads
    /// only; the caller runs the drain outside any connection lock.
    pub fn pending_assist_job_notifications(&self, limit: usize) -> Result<Vec<(i64, Value)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT seq, payload FROM events \
             WHERE kind='assist_intent' AND json_extract(payload,'$.status')='pending' \
             ORDER BY seq LIMIT ?",
        )?;
        let rows = stmt
            .query_map([limit as i64], |r| {
                let text: String = r.get(1)?;
                let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                Ok((r.get(0)?, value))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// CAD-1380 R4: move one pending assist intent to a terminal status
    /// (`delivered`, `refused` or `unresolved`) in one atomic UPDATE. The
    /// rest of the payload is preserved; only the `status` field changes,
    /// and only while the intent is still `pending`.
    pub fn finish_assist_job_notification(&self, seq: i64, outcome: &str) -> Result<()> {
        if !matches!(outcome, "delivered" | "refused" | "unresolved") {
            return Err(Error::internal("unknown assist intent outcome"));
        }
        self.write_tx(|tx| {
            let n = tx.execute(
                "UPDATE events SET payload=json_set(payload,'$.status',?) \
                 WHERE seq=? AND kind='assist_intent' \
                 AND json_extract(payload,'$.status')='pending'",
                params![outcome, seq],
            )?;
            if n == 0 {
                return Err(Error::rejected(format!(
                    "assist intent {seq} is not pending"
                )));
            }
            Ok(())
        })
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
                // CAD-1380 R4: a newly flagged stall also writes inert
                // `assist_intent` rows, keyed by this job's stall revision. The
                // guarded drain queues any private notice. No task fabricated:
                // the job's own initial task is the subject, else the stall
                // stays audit-only (the raw `assist_stall` event above).
                let initial: Option<String> = tx
                    .query_row(
                        "SELECT id FROM tasks WHERE job_id=? ORDER BY created LIMIT 1",
                        [job_id.as_str()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(task_id) = initial {
                    let job = self.job_in(tx, &job_id)?;
                    let task = self.task_in(tx, &task_id)?;
                    self.assist_intents_for_transition(
                        tx,
                        &job,
                        &task,
                        "stalled",
                        &format!("stall:{revision}"),
                    )?;
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

/// The proof-selected recipient of one assist intent, checked again inside
/// the final commit.
pub struct AssistRecipient {
    pub alias: String,
    pub job: String,
    pub org: String,
    pub sub: String,
    pub assistant_id: String,
}
