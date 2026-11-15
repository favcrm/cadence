//! CAD-1380/AOS-216: member personal-assistant bindings — the daemon-owned
//! record a verified `(org, sub)` member maps to. One row per
//! `(org, sub)`; `alias` names the daemon-registered assistant agent and
//! `conversation_id` the member's principal-scoped conversation. Rows are
//! written only by daemon-side provisioning (`assist_bind`), never by a
//! request field; the `member_as` relay param re-binds to these rows.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::*;

/// The owner-facing id of one accepted member job request. Derived from
/// the same length-prefixed `(org, sub, request_key)` triple the canonical
/// job id uses, under a domain-separated digest: stable across a replay,
/// a promotion and a crash, and never reused for a distinct principal or
/// key. The `request-` prefix keeps it disjoint from every job id.
pub fn assist_request_id(org: &str, sub: &str, request_key: &str) -> String {
    let digest = crate::operator_auth::digest(&format!(
        "assist-request|{}:{}|{}:{}|{}:{}",
        org.len(),
        org,
        sub.len(),
        sub,
        request_key.len(),
        request_key
    ));
    format!("request-{}", &digest[..digest.len().min(24)])
}

/// The canonical job id one request mints. Derived once, at acceptance,
/// from the same length-prefixed principal+key digest the request id uses,
/// so: a replay returns the same id, a crash-adoption finds its own
/// partially-created job, and a delivery reader can prove provenance from
/// the durable queue row even before the `assist_requests` ledger lands.
fn assist_job_id(org: &str, sub: &str, request_key: &str) -> String {
    let key_digest = crate::operator_auth::digest(&format!(
        "{}:{}|{}:{}|{}:{}",
        org.len(),
        org,
        sub.len(),
        sub,
        request_key.len(),
        request_key
    ));
    format!("assist-{}", &key_digest[..key_digest.len().min(24)])
}

/// CAD-2215: a durable accepted member job request. One row per
/// `(org, sub, request_key)`, written BEFORE any job publication so an
/// occupied issue yields a pending request instead of an
/// occupancy-dependent refusal. `job` is derived from the canonical
/// `assist_requests` ledger at read time, so acceptance and promotion can
/// never disagree after a crash: a job row without its ledger is adopted
/// by the request's own deterministic job id, and a ledger row is one job.
#[derive(Debug, Clone)]
pub struct AssistRequestQueued {
    pub org: String,
    pub sub: String,
    pub request_key: String,
    pub request_id: String,
    /// The canonical job id this request WILL mint, persisted at
    /// acceptance before any job row exists. It is never exposed to the
    /// member (the receipt has only `request_id`); it is the crash-proof
    /// provenance a delivery reader can match.
    pub planned_job_id: String,
    pub issue: String,
    pub title: String,
    pub body: String,
    pub acceptance: String,
    /// The issuing credential the request was minted under — the ORIGINAL
    /// provenance a later promotion re-proves, never the member's latest.
    pub credential_id: String,
    pub created: f64,
    /// Last autonomous promotion visit; ordering this first bounds
    /// starvation by permanently refused older rows.
    pub last_attempt: f64,
    /// Last sweep outcome for a still-pending request:
    /// `pending` (issue held / tracker busy), `refused` (authority,
    /// original-credential or service-bearer refusal) or `failed`.
    /// Never names another job or its occupancy.
    pub last_outcome: Option<String>,
    /// Durable completion flag: `0` until the canonical job's ledger bind
    /// AND its idempotent subscription/notice have all succeeded. A crash
    /// between the bind and those effects leaves `0`, so the worker keeps
    /// selecting the bound row and finishes it after re-proving the
    /// original credential.
    pub completed: bool,
    /// `Some(job)` once promoted; `None` while the issue is occupied.
    pub job: Option<String>,
}

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

pub(crate) enum PrivateJobReader<'a> {
    User { org: &'a str, sub: &'a str },
    Agent(&'a str),
    Unproven,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PrivateJobDenials {
    pub jobs: std::collections::BTreeSet<String>,
    pub tasks: std::collections::BTreeSet<String>,
    /// Issue-only messages cannot prove they are unrelated to these jobs.
    pub issues: std::collections::BTreeSet<String>,
    /// A human/anonymous relay cannot borrow the board's cloud identity.
    pub exclude_cloud: bool,
}

impl PrivateJobDenials {
    pub(crate) fn hides_message(&self, message: &super::Message) -> bool {
        match message.task_id.as_deref() {
            Some(task) => self.tasks.contains(task),
            None => message
                .issue
                .as_deref()
                .is_some_and(|issue| self.issues.contains(issue)),
        }
    }
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

/// The owner-facing accepted-request projection: the queue row plus the
/// canonical job (`NULL` while pending) derived from `assist_requests`.
const QUEUED_SELECT: &str = "SELECT q.org AS org, q.sub AS sub,
        q.request_key AS request_key, q.issue AS issue, q.title AS title,
        q.body AS body, q.acceptance AS acceptance,
        q.credential_id AS credential_id, q.created AS created,
        q.last_attempt AS last_attempt, q.last_outcome AS last_outcome,
        q.completed AS completed, q.job_id AS planned_job_id, r.job AS job
     FROM assist_request_queue q
     LEFT JOIN assist_requests r
       ON r.org=q.org AND r.sub=q.sub AND r.request_key=q.request_key";

fn row_queued(row: &rusqlite::Row) -> rusqlite::Result<AssistRequestQueued> {
    let org: String = row.get("org")?;
    let sub: String = row.get("sub")?;
    let request_key: String = row.get("request_key")?;
    Ok(AssistRequestQueued {
        request_id: assist_request_id(&org, &sub, &request_key),
        planned_job_id: row.get("planned_job_id")?,
        org,
        sub,
        request_key,
        issue: row.get("issue")?,
        title: row.get("title")?,
        body: row.get("body")?,
        acceptance: row.get("acceptance")?,
        credential_id: row.get("credential_id")?,
        created: row.get("created")?,
        last_attempt: row.get("last_attempt")?,
        last_outcome: row.get("last_outcome")?,
        completed: row.get::<_, i64>("completed")? != 0,
        job: row.get("job")?,
    })
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
    /// existing subscriber rows remain recorded but cannot read until sharing
    /// resumes and live member authority is re-proven.
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

    /// The one admissibility rule for member-private job provenance, shared
    /// by the owner-facing `member_job_list` projection and
    /// `private_job_denials`, so the two can never disagree about which
    /// jobs are readable.
    ///
    /// `bound` yields one row per job named by either durable record, with
    /// `provenance=1` only when those records agree exactly:
    /// * one unique `assist_requests` row (the owner binding) AND exactly
    ///   one `assist_request_queue` row for its `(org, sub, request_key)`
    ///   that names this job, is durably `completed=1`, and records the
    ///   job's ACTUAL `issue_id` (so a moved or malformed job cannot be
    ///   made readable by stale queue provenance); or
    /// * the pre-queue LEGACY case: one unique ledger row, no queue row for
    ///   this job, and none for its triple.
    ///
    /// A queue-only job (crash after the job row but before the ledger
    /// bind), a bound-but-incomplete queue row (`completed=0`), a planned
    /// job id that disagrees with the ledger, a job whose issue moved away
    /// from the recorded request issue, and ambiguous ledger or queue
    /// candidates all yield `provenance=0` and are therefore readable by no
    /// non-operator reader.
    const PRIVATE_PROVENANCE_CTE: &str = "owners AS (
        SELECT job, MIN(org) AS org, MIN(sub) AS sub,
          MIN(request_key) AS request_key, COUNT(*) AS n
        FROM assist_requests GROUP BY job
     ), queued AS (
        SELECT job_id AS job, COUNT(*) AS n
        FROM assist_request_queue GROUP BY job_id
     ), claims AS (
        SELECT o.job AS job,
          (SELECT COUNT(*) FROM assist_request_queue q
            WHERE q.org=o.org AND q.sub=o.sub
              AND q.request_key=o.request_key) AS claimed,
          (SELECT COUNT(*) FROM assist_request_queue q
            WHERE q.org=o.org AND q.sub=o.sub
              AND q.request_key=o.request_key
              AND q.job_id=o.job AND q.completed=1
              AND q.issue=j.issue_id) AS accepted
        FROM owners o JOIN jobs j ON j.id=o.job
     ), scoped AS (
        SELECT job FROM owners
        UNION
        SELECT job FROM queued
     ), bound AS (
        SELECT j.id AS job, j.issue_id AS issue, j.pm_alias AS pm_alias,
          o.org AS org, o.sub AS sub, COALESCE(o.n, 0) AS owners,
          CASE
            WHEN o.job IS NULL THEN 0
            WHEN o.n<>1 THEN 0
            WHEN COALESCE(c.claimed, 0)=1
                 AND COALESCE(c.accepted, 0)=1
                 AND COALESCE(q.n, 0)=1 THEN 1
            WHEN COALESCE(c.claimed, 0)=0
                 AND COALESCE(q.n, 0)=0 THEN 1
            ELSE 0
          END AS provenance
        FROM scoped s
        JOIN jobs j ON j.id=s.job
        LEFT JOIN owners o ON o.job=s.job
        LEFT JOIN queued q ON q.job=s.job
        LEFT JOIN claims c ON c.job=s.job
     )";

    /// Member job IDs whose project binding and current share are readable.
    /// Only a job with trusted provenance (see `PRIVATE_PROVENANCE_CTE`) is
    /// listed: a request that crashed before its ledger bind or before
    /// `completed=1` never appears in the owner's job list while it is
    /// still pending, so this projection agrees with the read denials.
    pub(crate) fn assist_member_jobs_scoped(
        &self,
        org: &str,
        sub: &str,
        issues: &[&str],
    ) -> Result<Vec<String>> {
        let conn = self.conn();
        let issues = serde_json::to_string(issues)?;
        let provenance = Self::PRIVATE_PROVENANCE_CTE;
        let mut stmt = conn.prepare(&format!(
            "WITH {provenance}
             SELECT b.job FROM bound b
             WHERE b.provenance=1 AND b.org=?1
               AND b.issue IN (SELECT value FROM json_each(?3))
               AND ((b.sub=?2) OR (EXISTS (
                    SELECT 1 FROM job_subscribers s
                    WHERE s.job=b.job AND s.org=?1 AND s.sub=?2
               ) AND EXISTS (
                    SELECT 1 FROM assist_job_shares h WHERE h.job=b.job AND h.org=?1
               )))
             ORDER BY b.job"
        ))?;
        let rows = stmt.query_map(params![org, sub, issues], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// A job has a usable owner only when its request provenance is unique
    /// AND settled: one ledger row whose queue row (when one exists) names
    /// this job, agrees on `(org, sub, request_key)` and is durably
    /// `completed=1`. A queue-only job, an interrupted bind (`completed=0`),
    /// a mismatched planned job id and ambiguous candidates all answer
    /// `None`, so a guessed job id cannot read or mutate a half-promoted
    /// job. A pre-queue legacy job without any queue row keeps its unique
    /// ledger owner. `assist_request_owns` stays raw provenance for the
    /// promotion/recovery path.
    pub(crate) fn assist_job_owner(&self, job: &str) -> Result<Option<(String, String)>> {
        let conn = self.conn();
        let provenance = Self::PRIVATE_PROVENANCE_CTE;
        let mut stmt = conn.prepare(&format!(
            "WITH {provenance}
             SELECT b.org, b.sub FROM bound b
             WHERE b.job=?1 AND b.provenance=1"
        ))?;
        let mut owners = stmt.query_map([job], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let first = owners.next().transpose()?;
        if owners.next().transpose()?.is_some() {
            return Ok(None);
        }
        Ok(first)
    }

    /// Snapshot private jobs and tasks this already-verified reader cannot
    /// read. Task assignment grants only that task, never the whole job.
    ///
    /// A job carries a TRUSTED provenance only when both durable records
    /// agree exactly: one `assist_requests` row, one `assist_request_queue`
    /// row naming the same `(org, sub, request_key)`, that queue row's
    /// planned canonical job id equal to the ledger job, and that queue row
    /// durably `completed=1` (the bind AND its idempotent subscription /
    /// notice effects all landed). A job with no queue row at all keeps the
    /// pre-queue legacy rule: one unique ledger owner is trusted.
    ///
    /// Everything else is untrusted and therefore denied to every
    /// non-operator reader — a queue-only job (a crash after the job row but
    /// before the ledger bind), a bound-but-incomplete queue row
    /// (`completed=0`), a queue row whose planned job disagrees with the
    /// ledger, ambiguous ledger or queue candidates for the same job, and
    /// (via `owners`) any job without one exact owner. Untrusted provenance
    /// revokes the task-assignee exception too: a half-promoted job must not
    /// reveal its tasks. The operator never reaches this query, so it stays
    /// recoverable.
    pub(crate) fn private_job_denials(
        &self,
        reader: PrivateJobReader<'_>,
    ) -> Result<PrivateJobDenials> {
        let conn = self.conn();
        self.private_job_denials_in(&conn, reader)
    }

    /// [`Self::private_job_denials`] on an already-open connection or write
    /// transaction, so an acknowledgement re-evaluates the grant/share state
    /// in the transaction that mutates the alert rather than against a
    /// snapshot captured before it.
    pub(crate) fn private_job_denials_in(
        &self,
        conn: &impl StoreConn,
        reader: PrivateJobReader<'_>,
    ) -> Result<PrivateJobDenials> {
        let (kind, principal, subject) = match reader {
            PrivateJobReader::User { org, sub } => ("user", org, sub),
            PrivateJobReader::Agent(alias) => ("agent", alias, ""),
            PrivateJobReader::Unproven => ("none", "", ""),
        };
        let provenance = Self::PRIVATE_PROVENANCE_CTE;
        #[allow(clippy::type_complexity)] // one query, one row shape
        let rows: Vec<(
            String,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
        )> = conn.query_vec(
            &format!(
                "WITH {provenance}, decisions AS (
                SELECT b.job AS job, b.owners AS owners, b.issue AS issue,
                  b.provenance AS provenance,
                  CASE WHEN b.provenance=1 AND (
                    (?1='user' AND b.org=?2 AND (
                      (b.sub=?3 AND EXISTS (
                         SELECT 1 FROM assistants a
                         WHERE a.org=b.org AND a.sub=b.sub AND a.state='active'
                      )) OR EXISTS (
                         SELECT 1 FROM job_subscribers s
                         JOIN assistants a ON a.org=s.org AND a.sub=s.sub AND a.state='active'
                         JOIN assist_job_shares h ON h.job=s.job AND h.org=s.org
                         WHERE s.job=b.job AND s.org=?2 AND s.sub=?3
                      )
                    )) OR (?1='agent' AND (
                      b.pm_alias=?2 OR EXISTS (
                         SELECT 1 FROM assistants a
                         WHERE a.org=b.org AND a.sub=b.sub AND a.alias=?2 AND a.state='active'
                      ) OR EXISTS (
                         SELECT 1 FROM job_subscribers s
                         JOIN assistants a ON a.org=s.org AND a.sub=s.sub AND a.state='active'
                         JOIN assist_job_shares h ON h.job=s.job AND h.org=s.org
                         WHERE s.job=b.job AND s.org=b.org AND a.alias=?2
                      )
                    ))
                  ) THEN 1 ELSE 0 END AS readable
                FROM bound b
             )
             SELECT d.job, d.owners, t.id, t.assignee, d.issue, d.provenance
             FROM decisions d LEFT JOIN tasks t ON t.job_id=d.job
             WHERE d.readable=0"
            ),
            params![kind, principal, subject],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )?;
        let mut denied = PrivateJobDenials::default();
        for (job, owners, task, assignee, issue, provenance) in rows {
            denied.jobs.insert(job);
            if let Some(issue) = issue {
                denied.issues.insert(issue);
            }
            if let Some(task) = task {
                // The assignee exception needs a trusted, exactly-matched
                // provenance: an interrupted promotion reveals no task.
                if provenance != 1
                    || owners != 1
                    || kind != "agent"
                    || assignee.as_deref() != Some(principal)
                {
                    denied.tasks.insert(task);
                }
            }
        }
        Ok(denied)
    }

    /// Did `(org, sub)` create `job` through a recorded request — the owner
    /// side of the read ACL (with `job_is_subscriber`).
    pub fn assist_request_owns(&self, org: &str, sub: &str, job: &str) -> Result<bool> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT 1 FROM assist_requests WHERE job=? AND org=? AND sub=? LIMIT 1")?;
        stmt.exists(params![job, org, sub]).map_err(Into::into)
    }

    /// Does `job` have ANY member-request provenance — a canonical
    /// `assist_requests` ledger row OR the durable accepted queue row whose
    /// planned canonical job id names it (including incomplete and completed
    /// requests)? Exact indexed `EXISTS` over both, so duplicate/ambiguous
    /// ledger rows still answer `true`, and a crash after job creation but
    /// before the ledger bind is still provably member-owned. A delivery
    /// reader must treat `true` as member-private and fail closed; this API
    /// never infers provenance from an id prefix. DB errors propagate.
    pub(crate) fn assist_job_has_request(&self, job: &str) -> Result<bool> {
        let conn = self.conn();
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM assist_requests WHERE job=?1
                 UNION ALL
                 SELECT 1 FROM assist_request_queue WHERE job_id=?1
             )",
            params![job],
            |row| row.get::<_, i64>(0),
        )
        .map(|found| found != 0)
        .map_err(Into::into)
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

    // ---- durable accepted request queue (CAD-2215) --------------------

    /// Accept one member job request durably, before any job publication.
    /// `INSERT OR IGNORE` makes the `(org, sub, request_key)` triple the
    /// single acceptance point: a replay never rewrites the winner's text
    /// and never retargets its issue — a replay naming a different issue
    /// refuses inside this transaction, with no write. Returns the stored
    /// record (the acceptance winner), whose `job` is derived from the
    /// canonical ledger.
    #[allow(clippy::too_many_arguments)]
    pub fn assist_request_enqueue(
        &self,
        org: &str,
        sub: &str,
        request_key: &str,
        issue: &str,
        title: &str,
        body: &str,
        acceptance: &str,
        credential_id: &str,
    ) -> Result<AssistRequestQueued> {
        let now = crate::issue::time::now_epoch() as f64;
        // Derive the canonical job id ONCE, at acceptance, so the queue row
        // names the job before any job row exists. A crash after creation
        // but before the ledger bind therefore remains provably this
        // member's, and promotion adopts exactly this id.
        let planned_job_id = assist_job_id(org, sub, request_key);
        self.write_tx(|tx| {
            // `INSERT OR IGNORE` is the cross-process single-acceptance
            // point: a concurrent equivalent (same triple) is a no-op, and
            // the read-back below is the committed winner. A retry with a
            // different issue refuses without rewriting the winner.
            tx.execute(
                "INSERT OR IGNORE INTO assist_request_queue(
                     org, sub, request_key, issue, title, body, acceptance,
                     credential_id, created, last_attempt, job_id)
                 VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    org,
                    sub,
                    request_key,
                    issue,
                    title,
                    body,
                    acceptance,
                    credential_id,
                    now,
                    now,
                    planned_job_id
                ],
            )?;
            let stored = Self::queued_in(&*tx, org, sub, request_key)?;
            if stored.issue != issue {
                return Err(Error::rejected(
                    "member job request replay refused: this request_key is already \
                     recorded against a different issue",
                ));
            }
            Ok(stored)
        })
    }

    /// Record one autonomous promotion visit (and its truthful outcome) so
    /// the bounded sweep rotates instead of retrying the same oldest
    /// refused rows forever, and so the owner can observe why a pending
    /// request is not progressing.
    pub fn assist_request_attempted(
        &self,
        org: &str,
        sub: &str,
        request_key: &str,
        outcome: &str,
    ) -> Result<()> {
        let now = crate::issue::time::now_epoch() as f64;
        self.write_tx(|tx| {
            tx.execute(
                "UPDATE assist_request_queue SET last_attempt=?, last_outcome=? \
                 WHERE org=? AND sub=? AND request_key=?",
                params![now, outcome, org, sub, request_key],
            )?;
            Ok(())
        })
    }

    /// Mark a bound request fully finished: the canonical ledger, the
    /// requester's subscription and the inert notice are all durable.
    /// Idempotent, so a crash after the effects but before this update
    /// converges on the next sweep.
    pub fn assist_request_complete(&self, org: &str, sub: &str, request_key: &str) -> Result<()> {
        self.write_tx(|tx| {
            tx.execute(
                "UPDATE assist_request_queue SET completed=1 \
                 WHERE org=? AND sub=? AND request_key=?",
                params![org, sub, request_key],
            )?;
            Ok(())
        })
    }

    /// Is `record` still the single unambiguous promotion target for its
    /// planned canonical job? Read-only — it writes nothing — and the
    /// caller runs it under `member_job_lock` before ANY promotion effect
    /// and again before final completion.
    ///
    /// It re-reads the durable queue row and requires it to be exactly the
    /// recorded one (same identity, issue, planned job id and original
    /// credential) and still incomplete (`completed=0`). It requires
    /// exactly one queued request to claim the planned job id, and the
    /// ownership ledger for either this identity or this planned job id to
    /// be absent (a queue-only promotion) or exactly one row naming the
    /// same identity AND the same planned job (an interrupted already-bound
    /// recovery). A retargeted or duplicated ledger row, a second request
    /// claiming the planned id, a rewritten queue row, or a row another
    /// promotion already completed all answer `false`, so no effect is
    /// committed against a different request. `completed=1` is not
    /// required for admission; only the effects of a still-unfinished
    /// request may proceed.
    pub(crate) fn assist_queue_promotion_unambiguous(
        &self,
        record: &AssistRequestQueued,
    ) -> Result<bool> {
        if record.planned_job_id.is_empty() {
            return Ok(false);
        }
        let conn = self.conn();
        let (identity, claims, ledger_rows, bound): (i64, i64, i64, i64) = conn.query_row(
            "SELECT
               (SELECT COUNT(*) FROM assist_request_queue q
                 WHERE q.org=?1 AND q.sub=?2 AND q.request_key=?3
                   AND q.issue=?4 AND q.job_id=?5 AND q.credential_id=?6
                   AND q.completed=0),
               (SELECT COUNT(*) FROM assist_request_queue q WHERE q.job_id=?5),
               (SELECT COUNT(*) FROM assist_requests r
                 WHERE r.job=?5 OR (r.org=?1 AND r.sub=?2 AND r.request_key=?3)),
               (SELECT COUNT(*) FROM assist_requests r
                 WHERE r.job=?5 AND r.org=?1 AND r.sub=?2 AND r.request_key=?3)",
            params![
                record.org.as_str(),
                record.sub.as_str(),
                record.request_key.as_str(),
                record.issue.as_str(),
                record.planned_job_id.as_str(),
                record.credential_id.as_str()
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        Ok(identity == 1 && claims == 1 && (ledger_rows == 0 || (ledger_rows == 1 && bound == 1)))
    }

    /// The accepted request for `(org, sub, request_key)`, or `None`.
    pub fn assist_request_queued(
        &self,
        org: &str,
        sub: &str,
        request_key: &str,
    ) -> Result<Option<AssistRequestQueued>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "{QUEUED_SELECT} WHERE q.org=? AND q.sub=? AND q.request_key=?"
        ))?;
        let mut rows = stmt.query_map(params![org, sub, request_key], row_queued)?;
        let first = rows.next().transpose()?;
        if rows.next().transpose()?.is_some() {
            return Err(Error::internal("duplicate accepted member job request"));
        }
        Ok(first)
    }

    /// Every accepted request this owner holds, oldest first. Owner-only:
    /// the caller has already proved the signed member and filters the
    /// rows by its current project scope.
    pub fn assist_requests_queued(&self, org: &str, sub: &str) -> Result<Vec<AssistRequestQueued>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "{QUEUED_SELECT} WHERE q.org=? AND q.sub=? ORDER BY q.created, q.request_key"
        ))?;
        let rows = stmt.query_map(params![org, sub], row_queued)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Pending accepted requests — those not yet fully finished — oldest
    /// attempt first, bounded. Includes a bound row whose subscription/notice
    /// effects did not complete before a crash (`completed=0`), so the
    /// worker's already-bound recovery branch is reachable. A fully finished
    /// request is `completed=1` and no longer selected.
    pub fn assist_requests_pending(&self, limit: usize) -> Result<Vec<AssistRequestQueued>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "{QUEUED_SELECT} WHERE q.completed=0 \
             ORDER BY q.last_attempt, q.created, q.request_key LIMIT ?"
        ))?;
        let rows = stmt.query_map([limit as i64], row_queued)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn queued_in(
        conn: &impl StoreConn,
        org: &str,
        sub: &str,
        request_key: &str,
    ) -> Result<AssistRequestQueued> {
        conn.query_opt(
            &format!("{QUEUED_SELECT} WHERE q.org=? AND q.sub=? AND q.request_key=?"),
            params![org, sub, request_key],
            row_queued,
        )?
        .ok_or_else(|| Error::internal("accepted member job request vanished"))
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
        let mut stmt = conn.prepare(
            "SELECT org, sub, request_key, job, title, body, acceptance,
                    credential_id, authority_rev, state, created
             FROM assist_requests WHERE job=? LIMIT 2",
        )?;
        let mut rows = stmt.query_map([job], |r| {
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
        })?;
        let first = rows.next().transpose()?;
        if rows.next().transpose()?.is_some() {
            return Err(crate::error::Error::rejected(
                "ambiguous member job ownership",
            ));
        }
        Ok(first)
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
