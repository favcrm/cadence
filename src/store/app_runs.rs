//! Project-free, broker-local text runs. This is not provider tool confinement.
use super::StoreConn;
use super::*;
use crate::app_run_workflow::{LocalStep, LocalWorkflow};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) mod artifact_actions;
mod authority;
mod creation;
mod dispatch;
mod host_settlement;
mod installation_consent;
mod queries;
pub(super) mod worker_completion;

/// CAD-622: one run's frozen membership as the app-team guard reads it.
/// `roles` is `Some` for the team-role map this build freezes (`role` →
/// agent alias, an explicitly empty map included); `None` for a snapshot
/// written before that member existed, whose role keys were never
/// recorded. `inputs` is the run's whole frozen input map — content, and
/// in a legacy snapshot the team-role bindings merged into it. The guard
/// never reads a content input as an agent; a legacy snapshot's inputs
/// are kept only so a question naming one of their values can hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunTeam {
    /// The snapshot's own run id, for a hold that names the run.
    pub id: String,
    /// The run's lifecycle state (`awaiting_approval`, `approved`,
    /// `running`, `succeeded`, `failed`, `cancelled`).
    pub state: String,
    /// The frozen role→alias map, or `None` when the snapshot predates it.
    pub roles: Option<BTreeMap<String, String>>,
    /// The frozen input map.
    pub inputs: BTreeMap<String, String>,
    /// The aliases the run assigned to its steps.
    pub assigned: Vec<String>,
    /// False only when the frozen workflow ran on the host, which takes
    /// no team: such a legacy input map is provably content.
    pub may_have_roles: bool,
    /// True when the operator explicitly retired this terminal legacy
    /// run's unrecorded provenance (`app_run_retire_legacy`): its input
    /// values no longer hold the app-team check.
    pub legacy_retired: bool,
}

pub const ARTIFACT_BYTES: usize = 256 * 1024;
pub const RUN_ARTIFACT_BYTES: usize = 1024 * 1024;

/// CAD-1123 R4: the stream that keeps each app run's approval record.
/// Not an agent alias and not the pruned daemon stream, so it survives.
pub const APP_RUN_APPROVAL_STREAM: &str = "app-run-approvals";

/// CAD-622: the durable event that records an operator's explicit
/// withdrawal of a terminal legacy run's unrecorded role provenance. A
/// terminal run cannot be cancelled, so this is its retirement; it lives
/// on the run-indexed approval stream, which is never pruned.
pub const APP_RUN_LEGACY_RETIRED: &str = "app_run_legacy_retired";

/// CAD-1374: a domain-specific HUMAN receipt. `None` (autonomous or local
/// operator) returns the legacy payload unchanged. The helper never copies
/// params, inputs, cookies or hashes.
fn human_receipt(
    payload: Value,
    human: Option<&Value>,
    operation: &str,
    action: &str,
    resource: &str,
    outcome: &str,
) -> Value {
    match human {
        Some(human) => canonical_receipt(
            payload,
            human.clone(),
            human["company"].clone(),
            operation,
            action,
            resource,
            outcome,
        ),
        None => payload,
    }
}

/// CAD-1374: the canonical receipt frame every app-run owner audit row carries.
/// `request_id` and `correlation_id` are the mutation's one server operation
/// UUID, and `causation_id` is null. `revision_or_digest` is the existing
/// snapshot or install digest from the payload. The reason is the action.
fn canonical_receipt(
    mut payload: Value,
    human: Value,
    company: Value,
    operation: &str,
    action: &str,
    resource: &str,
    outcome: &str,
) -> Value {
    let revision = payload
        .get("snapshot_digest")
        .or_else(|| payload.get("digest"))
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(map) = payload.as_object_mut() {
        map.insert("schema_version".into(), json!(1));
        map.insert("request_id".into(), json!(operation));
        map.insert("correlation_id".into(), json!(operation));
        map.insert("causation_id".into(), Value::Null);
        map.insert("company".into(), company);
        map.insert("human".into(), human);
        map.insert("action".into(), json!(action));
        map.insert("resource".into(), json!(resource));
        map.insert("outcome".into(), json!(outcome));
        map.insert("reason".into(), json!(action));
        map.insert("revision_or_digest".into(), revision);
    }
    payload
}

/// CAD-1374: the receipt for one dispatched step. `acting` is the manual owner
/// who clicked, or null for an automatic advance (`actor_kind` "system"). The
/// sealed approver is kept as `initiating_human` and is never credited as the
/// executing human. With neither a sealed approver nor an actor (legacy) no
/// human is invented and the payload stays unchanged.
fn dispatch_receipt(
    mut payload: Value,
    initiating: Option<&Value>,
    acting: Option<&Value>,
    operation: &str,
    resource: &str,
    outcome: &str,
) -> Value {
    let Some(company) = acting.or(initiating).map(|h| h["company"].clone()) else {
        return payload;
    };
    let (actor_kind, human) = match acting {
        Some(acting) => ("human", acting.clone()),
        None => ("system", Value::Null),
    };
    if let Some(map) = payload.as_object_mut() {
        map.insert(
            "initiating_human".into(),
            initiating.cloned().unwrap_or(Value::Null),
        );
        map.insert("actor_kind".into(), json!(actor_kind));
    }
    canonical_receipt(
        payload,
        human,
        company,
        operation,
        "app_run_step_dispatched",
        resource,
        outcome,
    )
}

/// CAD-1374: the human a stored approval recorded. Missing or null is a
/// genuine legacy approval. A present value must be a complete canonical
/// human (`sub`, `company`, `session` non-empty strings, a `role` of
/// `operator` or `member`, and a nullable `display`). Anything else fails
/// closed: no partial proof is ever read as legacy.
fn approval_human(recorded: Option<&Value>) -> Result<Value> {
    let Some(human) = recorded.and_then(|v| v.get("human")) else {
        return Ok(Value::Null);
    };
    if human.is_null() {
        return Ok(Value::Null);
    }
    let text = |key: &str| {
        human[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::internal("approval human binding is malformed"))
    };
    text("sub")?;
    text("company")?;
    text("session")?;
    if !matches!(human["role"].as_str(), Some("operator" | "member")) {
        return Err(Error::internal("approval human binding is malformed"));
    }
    if !human["display"].is_null() && !human["display"].is_string() {
        return Err(Error::internal("approval human binding is malformed"));
    }
    Ok(json!({
        "sub": human["sub"],
        "company": human["company"],
        "session": human["session"],
        "role": human["role"],
        "display": human["display"],
    }))
}
pub fn artifact_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
pub fn material_digest(value: &Value) -> String {
    cadence_core::connection_dispatch::material_digest(value)
}

/// The exact kickoff body one step is dispatched with.
///
/// Creation measures this same body — for every step, with the run id it is
/// about to write — plus [`KICKOFF_DEPENDENCY_BYTES`] per declared dependency,
/// and refuses before any run row exists when it could not be transported.
/// Dispatch sends the body this builds from the SNAPSHOT alone: `skill` is the
/// material frozen at creation, never a re-read of the live bundle.
fn kickoff_body(
    run_id: &str,
    step: &LocalStep,
    dependencies: &[Value],
    snapshot: &Value,
) -> Result<String> {
    let envelope = json!({"schema":1,"run_id":run_id,"step_id":step.id,"revision":1,"kind":step.kind,"instruction":step.instruction,"dependencies":dependencies,
        "skill":step.skill,
        "source":snapshot["source"],
        "capability_slots":snapshot["workflow"]["capability_slots"],
        "required_asset_slot":snapshot["workflow"]["required_asset_slot"],
        "result_contract":"Return exactly one complete JSON envelope as your final text, with no prose, heading, or Markdown fence. It must have schema=1, kind, run_id, step_id, revision. Producer: outcome=succeeded, artifacts=[{media_type:text/markdown,text:...}]. Reviewer: producer_step_id, producer_revision=1, artifact_sha256, decision=approve|revise, rationale. If required_asset_slot is set, approval must include asset_receipt_id and asset_sha256 from that exact slot's fetched receipt; otherwise these fields are optional for a reviewed binary asset. Fetch dependencies with the active message and turn token. An approve is refused unless this turn successfully fetched that dependency; revise does not require the fetch. Managed Codex: call MCP self, then MCP app_run_artifact with the dependency artifact_id and that response's exact id as message and turn_id as token. Other providers: cadence app run artifact ID --message MSG --token TOKEN. No outward effects authorized.","max_artifact_bytes":ARTIFACT_BYTES});
    serde_json::to_string(&envelope).map_err(|e| Error::internal(e.to_string()))
}

/// Worst-case bytes one `dependencies` entry adds to the kickoff body: the
/// artifact id (`artifact-<run>-<step>`), the producer step id, the revision,
/// the `sha256:` digest, a media type and a byte count. Creation reserves this
/// per declared dependency because the artifact does not exist yet; dispatch
/// then measures the real body again.
const KICKOFF_DEPENDENCY_BYTES: usize = 320;

pub(super) const OPERATOR_DECISION_SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS app_run_dispositions(
 run_id TEXT PRIMARY KEY REFERENCES app_runs(id),
 request_id TEXT NOT NULL UNIQUE,
 artifact_id TEXT NOT NULL REFERENCES app_run_artifacts(id),
 artifact_digest TEXT NOT NULL,
 decision TEXT NOT NULL CHECK(decision IN ('accept','decline')),
 created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS app_run_tickets(
 run_id TEXT PRIMARY KEY REFERENCES app_runs(id),
 request_id TEXT NOT NULL UNIQUE,
 artifact_id TEXT NOT NULL REFERENCES app_run_artifacts(id),
 artifact_digest TEXT NOT NULL,
 suggestion_index INTEGER NOT NULL CHECK(suggestion_index>=1),
 suggestion_count INTEGER NOT NULL CHECK(suggestion_count>=2),
 suggestion_text TEXT NOT NULL CHECK(length(suggestion_text)>0),
 issue_id TEXT,
 created REAL NOT NULL,
 CHECK(issue_id IS NULL OR length(issue_id)>0));
CREATE UNIQUE INDEX IF NOT EXISTS app_run_tickets_issue
 ON app_run_tickets(issue_id) WHERE issue_id IS NOT NULL;
";

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_install_capabilities(
 install_id TEXT PRIMARY KEY, epoch INTEGER NOT NULL CHECK(epoch>0),
 digest TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('approved','revoked')),
 created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS app_runs(
 id TEXT PRIMARY KEY, install_id TEXT NOT NULL, epoch INTEGER NOT NULL,
 bundle_digest TEXT NOT NULL, snapshot TEXT NOT NULL, snapshot_digest TEXT NOT NULL,
 project_link TEXT, owner_pm TEXT NOT NULL, request_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('awaiting_approval','approved','running','succeeded','failed','cancelled')),
 approved_digest TEXT, created REAL NOT NULL, updated REAL NOT NULL,
 UNIQUE(install_id,request_id));
CREATE TABLE IF NOT EXISTS app_run_failures(
 run_id TEXT PRIMARY KEY REFERENCES app_runs(id), step_id TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('refused','uncertain')), reason TEXT NOT NULL,
 created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS app_run_steps(
 run_id TEXT NOT NULL REFERENCES app_runs(id), step_id TEXT NOT NULL,
 task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id), spec TEXT NOT NULL,
 identity_digest TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','dispatched','succeeded','failed')),
 message_id TEXT UNIQUE, result_digest TEXT,
 PRIMARY KEY(run_id,step_id));
CREATE TABLE IF NOT EXISTS app_run_artifacts(
 id TEXT PRIMARY KEY, run_id TEXT NOT NULL, step_id TEXT NOT NULL,
 message_id TEXT NOT NULL UNIQUE, turn_id TEXT NOT NULL, producer TEXT NOT NULL,
 digest TEXT NOT NULL, media_type TEXT NOT NULL, content BLOB NOT NULL,
 created REAL NOT NULL, UNIQUE(run_id,step_id),
 FOREIGN KEY(run_id,step_id) REFERENCES app_run_steps(run_id,step_id));
CREATE TABLE IF NOT EXISTS app_run_reviews(
 run_id TEXT NOT NULL, step_id TEXT NOT NULL, artifact_id TEXT NOT NULL REFERENCES app_run_artifacts(id),
 artifact_digest TEXT NOT NULL, reviewer TEXT NOT NULL, message_id TEXT UNIQUE NOT NULL,
 decision TEXT NOT NULL CHECK(decision IN ('approve','revise')), rationale TEXT NOT NULL,
 PRIMARY KEY(run_id,step_id), FOREIGN KEY(run_id,step_id) REFERENCES app_run_steps(run_id,step_id));
CREATE TABLE IF NOT EXISTS app_run_artifact_fetches(
 artifact_id TEXT NOT NULL REFERENCES app_run_artifacts(id),
 message_id TEXT NOT NULL, turn_hash TEXT NOT NULL, created REAL NOT NULL,
 PRIMARY KEY(artifact_id,message_id,turn_hash));
";

pub struct LocalRunProvenance<'a> {
    pub selected_source: Option<(&'a str, &'a str)>,
    pub input_origins: &'a BTreeMap<String, String>,
    /// CAD-622: the installation team's role → alias bindings this run
    /// froze into its inputs, kept as their own member so the app-team
    /// guard reads role provenance rather than guessing role names from
    /// aliases (or from content inputs).
    pub team_roles: &'a BTreeMap<String, String>,
    pub human: Option<&'a Value>,
}

impl Store {
    pub(super) fn refuse_app_task(conn: &impl super::StoreConn, task_id: &str) -> Result<()> {
        if conn
            .query_row(
                "SELECT 1 FROM app_run_steps WHERE task_id=?",
                [task_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(Error::rejected(
                "app-owned tasks use the app run lifecycle; generic task mutation is refused",
            ));
        }
        Ok(())
    }
    pub(super) fn refuse_app_job(conn: &impl super::StoreConn, job_id: &str) -> Result<()> {
        if conn.query_row("SELECT 1 FROM app_run_steps s JOIN tasks t ON t.id=s.task_id WHERE t.job_id=? LIMIT 1",[job_id], |_| Ok(())).optional()?.is_some() {
            return Err(Error::rejected("app-owned jobs use the app run lifecycle"));
        }
        Ok(())
    }
}

impl Store {
    pub fn app_run_decide(
        &self,
        id: &str,
        digest: Option<&str>,
        cancel: bool,
        current_bundle: Option<&str>,
    ) -> Result<Value> {
        self.app_run_decide_as(id, digest, cancel, current_bundle, None)
    }
    pub fn app_run_decide_as(
        &self,
        id: &str,
        digest: Option<&str>,
        cancel: bool,
        current_bundle: Option<&str>,
        human: Option<&Value>,
    ) -> Result<Value> {
        let operation = uuid::Uuid::new_v4().to_string();
        self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let run = Self::app_run_show_in(&tx, id)?;
                    let state = run["state"].as_str().unwrap();
                    if cancel {
                        if matches!(state, "succeeded" | "failed" | "cancelled") {
                            return Err(Error::rejected("run is already terminal"));
                        }
                        tx.execute(
                            "UPDATE app_runs SET state='cancelled',approved_digest=NULL,updated=? WHERE id=?",
                            params![now(), id],
                        )?;
                        tx.execute(
                            "UPDATE jobs SET state='cancelled',updated=? WHERE id=?",
                            params![now(), id],
                        )?;
                    } else {
                        if state != "awaiting_approval" || digest != run["snapshot_digest"].as_str() {
                            return Err(Error::rejected(
                                "execution decision needs the pending immutable snapshot digest",
                            ));
                        }
                        Self::app_current_in(
                            &tx,
                            &run,
                            current_bundle.ok_or_else(|| {
                                Error::rejected("execution approval requires current installation proof")
                            })?,
                        )?;
                        tx.execute("UPDATE app_runs SET state='approved',approved_digest=snapshot_digest,updated=? WHERE id=?",params![now(),id])?;
                        // CAD-1123 R4: a durable, run-indexed approval record on its
                        // own stream (the daemon stream is pruned). The actor is the
                        // daemon-derived class, never a caller-supplied name.
                        Self::event_scoped(
                            &tx,
                            APP_RUN_APPROVAL_STREAM,
                            "app_run_approved",
                            human_receipt(json!({"run_id":id,"digest":run["snapshot_digest"],"by":"operator"}), human, &operation, "app_run_approved", &format!("app-run:{id}"), "approved"),
                            Some(id),
                            None,
                        )?;
                    }
                    Self::event(
                        &tx,
                        Self::DAEMON_STREAM,
                        if cancel {
                            "app_run_cancelled"
                        } else {
                            "app_run_execution_approved"
                        },
                        human_receipt(json!({"run_id":id,"digest":run["snapshot_digest"],"actor":"operator"}), human, &operation, if cancel { "app_run_cancelled" } else { "app_run_approved" }, &format!("app-run:{id}"), if cancel { "cancelled" } else { "approved" }),
                    )?;
                    Self::app_run_show_in(&tx, id)
        })
    }

    /// CAD-622: retire a terminal legacy run's unrecorded role
    /// provenance. Its snapshot predates `team_roles`, so the app-team
    /// guard cannot tell which of its inputs were roles, and a terminal
    /// run cannot be cancelled (`app_run_decide_as` refuses that). The
    /// operator records an explicit withdrawal instead: a durable,
    /// run-indexed `app_run_legacy_retired` event. The lifecycle state
    /// and history are untouched; from the moment the event exists the
    /// guard stops holding that run's input values. Idempotent.
    pub fn app_run_retire_legacy(&self, id: &str, human: Option<&Value>) -> Result<Value> {
        let operation = uuid::Uuid::new_v4().to_string();
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let run = Self::app_run_show_in(&tx, id)?;
            if run["snapshot"].get("team_roles").is_some() {
                return Err(Error::rejected(
                    "only a run frozen before team-role provenance can be retired",
                ));
            }
            let state = run["state"].as_str().unwrap_or_default();
            if !matches!(state, "succeeded" | "failed") {
                return Err(Error::rejected(
                    "only a terminal run needs retirement — cancel a live run instead",
                ));
            }
            let retired = tx
                .query_row(
                    "SELECT 1 FROM events WHERE job_id=?1 AND alias=?2 AND kind=?3 LIMIT 1",
                    params![id, APP_RUN_APPROVAL_STREAM, APP_RUN_LEGACY_RETIRED],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !retired {
                Self::event_scoped(
                    &tx,
                    APP_RUN_APPROVAL_STREAM,
                    APP_RUN_LEGACY_RETIRED,
                    human_receipt(
                        json!({"run_id":id,"snapshot_digest":run["snapshot_digest"],"by":"operator"}),
                        human,
                        &operation,
                        APP_RUN_LEGACY_RETIRED,
                        &format!("app-run:{id}"),
                        "retired",
                    ),
                    Some(id),
                    None,
                )?;
            }
            Self::app_run_show_in(&tx, id)
        })
    }
}

impl Store {
    /// CAD-1120: the identity an app run binds a worker to — the
    /// registered agent and its native session, not one endpoint process.
    /// `generation` is left out: every open mints a new one, so binding
    /// it would refuse a worker that the idle timer stopped and dispatch
    /// woke. Each turn is still bound to the live generation by
    /// `local_token_current`.
    pub(super) fn app_binding_identity(agent: &Agent) -> Value {
        let mut identity = Self::agent_identity(agent);
        if let Some(fields) = identity.as_object_mut() {
            fields.remove("generation");
        }
        identity
    }

    /// CAD-1120: whether `agent` is parked by the idle timer rather than
    /// stopped by a person. It must be disabled and `stopped`, and its
    /// newest stop record must be the timer's own `agent_auto_stopped`.
    /// An operator or PM stop (`stop_requested`), an open (`ready`), or an
    /// auto-resume in flight or failed, written after it, makes this
    /// false. Only the daemon's idle timer writes that record, so no
    /// caller can mark a stopped worker as parked. A parked worker is
    /// woken by the CAD-413 auto-resume once its kickoff is queued.
    pub(super) fn agent_auto_parked_in(
        conn: &impl super::StoreConn,
        agent: &Agent,
    ) -> Result<bool> {
        if agent.enabled || agent.state != "stopped" {
            return Ok(false);
        }
        let placeholders = vec!["?"; AUTO_STOP_MARKER_KINDS.len()].join(",");
        let sql = format!(
            "SELECT kind FROM events WHERE alias=? AND kind IN ({placeholders})
             ORDER BY seq DESC LIMIT 1"
        );
        let mut args: Vec<&dyn rusqlite::ToSql> = vec![&agent.alias];
        args.extend(
            AUTO_STOP_MARKER_KINDS
                .iter()
                .map(|kind| kind as &dyn rusqlite::ToSql),
        );
        let newest: Option<String> = conn
            .query_row(&sql, args.as_slice(), |row| row.get(0))
            .optional()?;
        Ok(newest.as_deref() == Some(AUTO_STOP_EVENT))
    }
}

impl Store {
    /// CAD-1258: a worker capability call that reached the provider but could
    /// not retain its result (or failed after the claim) fails the run with a
    /// durable reason. The claim stays, so nothing is ever sent twice.
    pub fn app_run_call_failed(
        &self,
        run_id: &str,
        step_id: &str,
        kind: &str,
        reason: &str,
    ) -> Result<()> {
        if !matches!(kind, "refused" | "uncertain") || reason.is_empty() || reason.len() > 1024 {
            return Err(Error::rejected("capability failure record is invalid"));
        }
        self.write_tx(|conn| {
            let tx = &mut *conn;
            // Raw column, not `app_run_show_in`: the failure must record even
            // when the persisted snapshot is what could not be read.
            let state: String = tx.query_row(
                "SELECT state FROM app_runs WHERE id=?",
                [run_id],
                |r| r.get(0),
            )?;
            if state != "running" {
                return Ok(());
            }
            tx.execute(
                "INSERT INTO app_run_failures(run_id,step_id,kind,reason,created) VALUES(?,?,?,?,?)",
                params![run_id, step_id, kind, reason, now()],
            )?;
            // The failed call's own step is terminal even though its worker
            // message is still running (invalidate keeps that association).
            tx.execute(
                "UPDATE app_run_steps SET state='failed' WHERE run_id=? AND step_id=? AND state='dispatched'",
                params![run_id, step_id],
            )?;
            self.app_run_invalidate_in(&tx, run_id, reason)
        })
    }

    /// Authority loss is terminal; existing artifacts and turn receipts remain
    /// immutable. This never retries uncertain provider work. `reason` hints
    /// at the cause; only its safe classification (`app_reason_class`) is
    /// persisted on the failed steps — arbitrary error text interpolated
    /// from transport or provider output can carry credentials.
    pub fn app_run_invalidate(&self, id: &str, reason: &str) -> Result<()> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            self.app_run_invalidate_in(&tx, id, reason)?;
            Ok(())
        })
    }
    pub(super) fn app_run_invalidate_in(
        &self,
        tx: &impl super::StoreConn,
        id: &str,
        reason: &str,
    ) -> Result<()> {
        let changed = tx.execute("UPDATE app_runs SET state='failed',approved_digest=NULL,updated=? WHERE id=? AND state IN ('awaiting_approval','approved','running')", params![now(), id])?;
        if changed != 0 {
            tx.execute(
                "UPDATE jobs SET state='failed',updated=? WHERE id=?",
                params![now(), id],
            )?;
            // Keep an already running transport's association until its
            // authenticated completion records failure. The run predicate
            // already closes material/dispatch authority; erasing the step
            // association would roll back the transport's terminal receipt.
            tx.execute("UPDATE app_run_steps SET state='failed' WHERE run_id=? AND state IN ('pending','dispatched') AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.id=app_run_steps.message_id AND m.state='running')", [id])?;
            // CAD-1142: `reason` reaches here from a dispatch/binding
            // rejection's `Display` — free text, possibly interpolated
            // with provider output. Persist only its safe class; the
            // classified string is still human-readable and names the
            // authority kind (binding/price/assignment/context).
            let reason = app_reason_class(reason);
            tx.execute("UPDATE tasks SET state='failed',error=?,updated=? WHERE id IN (SELECT task_id FROM app_run_steps WHERE run_id=? AND state='failed')", params![reason, now(), id])?;
            tx.execute("UPDATE messages SET state='failed',error='app submission authorization changed',completed=? WHERE source='app_run_dispatch' AND state IN ('queued','submitting') AND id IN (SELECT message_id FROM app_run_steps WHERE run_id=?)",params![now(),id])?;
            Self::event(
                tx,
                Self::DAEMON_STREAM,
                "app_run_invalidated",
                json!({"run_id":id,"reason":"authority_or_assignment_changed"}),
            )?;
        }
        Ok(())
    }
    pub fn app_run_pending(&self) -> Result<Vec<(String, String, String)>> {
        Ok(self
            .conn()
            .prepare("SELECT id,install_id,bundle_digest FROM app_runs WHERE state='running'")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn app_artifact_with_digest(
        &self,
        id: &str,
        turn: Option<(&str, &str)>,
        current_bundle: &str,
    ) -> Result<Value> {
        let (message, token) =
            turn.ok_or_else(|| Error::rejected("worker artifact fetch requires an assigned turn"))?;
        // The existing worker guards and the byte, digest, and bounds checks
        // run in this sealed write. The receipt commits only after they pass;
        // a refusal rolls it back with them.
        self.write_tx(|tx| {
            let value =
                self.app_artifact_read_in(tx, id, Some((message, token)), Some(current_bundle))?;
            tx.execute(
                "INSERT INTO app_run_artifact_fetches(artifact_id,message_id,turn_hash,created) VALUES(?,?,?,?) ON CONFLICT(artifact_id,message_id,turn_hash) DO NOTHING",
                params![id, message, artifact_digest(token.as_bytes()), now()],
            )?;
            Ok(value)
        })
    }
    /// Caller proof is supplied by the daemon's operator-only route. Audit
    /// access is distinct from current execution or installation authority
    /// and does not record a worker fetch receipt.
    pub fn app_artifact_for_operator(&self, id: &str) -> Result<Value> {
        let conn = self.conn();
        self.app_artifact_read_in(&conn, id, None, None)
    }
    fn app_artifact_read_in(
        &self,
        conn: &impl super::StoreConn,
        id: &str,
        turn: Option<(&str, &str)>,
        current_bundle: Option<&str>,
    ) -> Result<Value> {
        let (run_id, producer_step, digest, media_type, bytes): (
            String,
            String,
            String,
            String,
            Vec<u8>,
        ) = conn
            .query_row(
                "SELECT a.run_id,a.step_id,a.digest,a.media_type,substr(a.content,1,?2) FROM app_run_artifacts a JOIN app_run_steps s ON s.run_id=a.run_id AND s.step_id=a.step_id JOIN app_runs r ON r.id=a.run_id WHERE a.id=?1",
                params![id, (ARTIFACT_BYTES + 1) as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?
            .ok_or_else(|| Error::rejected("artifact unavailable"))?;
        let run = Self::app_run_show_in(conn, &run_id)?;
        if let Some((message, token)) = turn {
            Self::app_current_in(
                conn,
                &run,
                current_bundle.ok_or_else(|| {
                    Error::rejected("worker fetch requires current installation proof")
                })?,
            )?;
            if run["state"] != "running" || run["approved_digest"] != run["snapshot_digest"] {
                return Err(Error::rejected(
                    "artifact run execution is no longer approved",
                ));
            }
            let msg = self
                .message_in(conn, message)?
                .filter(|m| m.state == "running" && m.turn_id.as_deref() == Some(token))
                .ok_or_else(|| Error::rejected("artifact needs an active assigned turn"))?;
            let spec:String=conn.query_row("SELECT spec FROM app_run_steps WHERE run_id=? AND message_id=? AND state='dispatched'",params![run_id,message],|r|r.get(0)).optional()?.ok_or_else(||Error::rejected("artifact is not a dependency of this turn"))?;
            let step: LocalStep =
                serde_json::from_str(&spec).map_err(|e| Error::internal(e.to_string()))?;
            let generation = self.agent_in(conn, &msg.alias)?.generation;
            let assigned = &run["snapshot"]["assignments"][&step.id];
            if step.assignee != msg.alias
                || material_digest(&Self::app_binding_identity(
                    &self.agent_in(conn, &msg.alias)?,
                )) != assigned["identity_digest"].as_str().unwrap()
                || !step.dependencies.contains(&producer_step)
                || !local_token_current(
                    assigned["provider"].as_str().unwrap(),
                    assigned["endpoint_kind"].as_str().unwrap(),
                    generation.as_deref(),
                    token,
                )
            {
                return Err(Error::rejected(
                    "artifact is not authorized for this assigned turn",
                ));
            }
        }
        if bytes.is_empty()
            || bytes.len() > ARTIFACT_BYTES
            || !matches!(media_type.as_str(), "text/plain" | "text/markdown")
            || artifact_digest(&bytes) != digest
        {
            return Err(Error::rejected(
                "persisted artifact integrity or bounds refused",
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::internal("stored text artifact is not UTF-8"))?;
        Ok(json!({"id":id,"digest":digest,"media_type":media_type,"size":text.len(),"text":text}))
    }
}

/// CAD-1142 reason privacy. A step's persisted reason — projected as
/// `steps[].reason` on run reads and carried by the `app_run_failed` /
/// `app_run_invalidated` daemon-stream payloads — is readable by any
/// `agent_events` caller (`Rule::Read`, including Unproven). Provider
/// error text (`messages.error`, `result.error`, managed Pi's verbatim
/// `errorMessage`) may carry credentials or prose, so it is never
/// copied onto those surfaces. Instead a free-text detail is classified
/// onto this bounded allowlist of Cadence-authored classes: the class
/// still names the real failure kind (binding, price, assignment,
/// context), while the raw detail stays on operator-only surfaces (the
/// message row and authorized run reads). An unrecognized detail
/// degrades to the generic authority-loss class — the class may be less
/// specific, but no provider string can escape through it.
pub(super) fn app_reason_class(detail: &str) -> &'static str {
    let lower = detail.to_lowercase();
    if lower.contains("binding") || lower.contains("receipt") {
        "run binding receipt is no longer current"
    } else if lower.contains("price") || lower.contains("quote") {
        "capability price could not be confirmed"
    } else if lower.contains("context") {
        "run context was updated or archived"
    } else if lower.contains("connection") {
        "capability connection receipt is stale or unavailable"
    } else if lower.contains("assignment") || lower.contains("identity") {
        "registered assignment identity or group changed"
    } else if lower.contains("approval") || lower.contains("digest") || lower.contains("bundle") {
        "run execution approval is absent or stale"
    } else {
        "app authority or assignment is no longer current"
    }
}

/// CAD-1142 reason privacy: classify how a worker's turn ended into a
/// bounded human-readable class. `status` is already restricted to the
/// terminal allowlist by `finish_in`/`reconcile`, so interpolating it
/// is safe; `detail` names only whether the provider reported error
/// prose at all, never the prose itself.
#[doc(hidden)]
pub fn app_worker_turn_reason(status: &str, detail: bool) -> String {
    match (status, detail) {
        ("failed", true) => "worker turn failed: provider error".to_string(),
        ("interrupted", _) => "worker turn was interrupted".to_string(),
        ("unknown", _) => {
            "worker turn outcome is uncertain; inspect authorized run surfaces".to_string()
        }
        (other, true) => format!("worker turn {other}: provider error"),
        (other, false) => format!("worker turn {other} without producing material"),
    }
}

/// CAD-1142 reason privacy: the bounded class an app-owned turn's
/// uncertain-outcome fence publishes on the public surfaces — the
/// worker's `attention` event and the agent row's `error`. The raw
/// provider account stays on the operator-private message row (app
/// mail inspection already requires operator proof); this class is
/// the only text Unproven and Agent readers ever see.
#[doc(hidden)]
pub fn app_uncertain_turn_reason() -> String {
    app_worker_turn_reason("unknown", true)
}

impl Store {
    fn app_step_failed_in(
        &self,
        tx: &impl super::StoreConn,
        run: &str,
        step: &str,
        task: &str,
        reason: &str,
    ) -> Result<()> {
        tx.execute(
            "UPDATE app_run_steps SET state='failed' WHERE run_id=? AND step_id=?",
            params![run, step],
        )?;
        // CAD-1142: callers pass classified or Cadence-literal reasons;
        // bound defensively so no caller can grow an unbounded string on
        // the projected step reason or the daemon-stream payload.
        let reason: String = reason.chars().take(256).collect();
        tx.execute(
            "UPDATE tasks SET state='failed',error=?,updated=? WHERE id=?",
            params![reason, now(), task],
        )?;
        tx.execute(
            "UPDATE app_runs SET state='failed',updated=? WHERE id=? AND state!='cancelled'",
            params![now(), run],
        )?;
        tx.execute(
            "UPDATE jobs SET state='failed',updated=? WHERE id=? AND state!='cancelled'",
            params![now(), run],
        )?;
        Self::event(
            tx,
            Self::DAEMON_STREAM,
            "app_run_failed",
            json!({"run_id":run,"step_id":step,"reason":reason}),
        )?;
        Ok(())
    }
}

impl Store {
    pub fn app_artifact_run_id(&self, id: &str) -> Result<String> {
        self.conn()
            .query_row(
                "SELECT run_id FROM app_run_artifacts WHERE id=?",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::rejected("artifact unavailable"))
    }
    pub fn app_artifact_installation(&self, id: &str) -> Result<String> {
        self.conn().query_row("SELECT r.install_id FROM app_run_artifacts a JOIN app_runs r ON r.id=a.run_id WHERE a.id=?",[id],|r|r.get(0)).optional()?.ok_or_else(||Error::rejected("artifact unavailable"))
    }
    pub fn app_task_owned(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row("SELECT 1 FROM app_run_steps WHERE task_id=?", [id], |_| {
                Ok(())
            })
            .optional()?
            .is_some())
    }
}

fn local_token_current(provider: &str, kind: &str, generation: Option<&str>, token: &str) -> bool {
    let Some(spec) = crate::adapter::registry::spec_opt(provider, kind) else {
        return false;
    };
    !token.is_empty()
        && (spec.turn_token.is_none()
            || crate::adapter::registry::turn_token_current(provider, kind, generation, token))
}

pub struct LocalRunRequest<'a> {
    pub install_id: &'a str,
    pub bundle_digest: &'a str,
    pub workflow: &'a LocalWorkflow,
    pub inputs: &'a BTreeMap<String, String>,
    pub request_id: &'a str,
    /// CAD-1171: required for an agent run; absent (None) for a host run.
    pub owner_pm: Option<&'a str>,
    pub project_link: Option<&'a str>,
}

impl Store {
    /// The socket caller and live turn select the run; a request cannot name
    /// another run, installation, context, account, or provider.
    #[doc(hidden)]
    pub fn app_capability_turn(
        &self,
        alias: &str,
        message: &str,
        token: &str,
        slot: &str,
        bundle: &str,
    ) -> Result<(Value, String, super::app_bindings::BindingProof)> {
        let conn = self.conn();
        let msg = self
            .message_in(&conn, message)?
            .filter(|m| {
                m.alias == alias
                    && m.source == "app_run_dispatch"
                    && m.state == "running"
                    && m.turn_id.as_deref() == Some(token)
            })
            .ok_or_else(|| Error::rejected("capability needs the active assigned app turn"))?;
        self.app_message_admit_in(&conn, &msg, bundle)?;
        let (run_id, step_id, spec): (String, String, String) = conn.query_row(
            "SELECT run_id,step_id,spec FROM app_run_steps WHERE message_id=? AND state='dispatched'",
            [message], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        )?;
        let step: LocalStep = serde_json::from_str(&spec)?;
        let run = Self::app_run_show_in(&conn, &run_id)?;
        let assigned = &run["snapshot"]["assignments"][&step_id];
        let generation = self.agent_in(&conn, alias)?.generation;
        if step.assignee != alias
            || !local_token_current(
                assigned["provider"].as_str().unwrap_or_default(),
                assigned["endpoint_kind"].as_str().unwrap_or_default(),
                generation.as_deref(),
                token,
            )
        {
            return Err(Error::rejected(
                "capability turn endpoint is no longer current",
            ));
        }
        if run["snapshot"]["schema"] != 4
            || !run["snapshot"]["workflow"]["capability_slots"]
                .as_array()
                .is_some_and(|slots| slots.iter().any(|value| value == slot))
        {
            return Err(Error::rejected(
                "slot is not in the frozen run capability set",
            ));
        }
        let proof: super::app_bindings::BindingProof =
            serde_json::from_value(run["snapshot"]["capabilities"][slot].clone())
                .map_err(|_| Error::rejected("frozen capability binding is absent"))?;
        if !matches!(
            proof.config["mapping"]["effect"].as_str(),
            Some("read" | "draft")
        ) {
            return Err(Error::rejected(
                "app capability effects are read or draft only",
            ));
        }
        Ok((run, step_id, proof))
    }

    pub fn app_message_installation(&self, message: &str) -> Result<Option<(String, String)>> {
        {
            self.read_tx(|conn| {

                            Ok(conn.query_opt("SELECT r.id,r.install_id FROM app_run_steps s JOIN app_runs r ON r.id=s.run_id WHERE s.message_id=?",[message],|r|Ok((r.get(0)?,r.get(1)?)))?)
            })
        }
    }
    pub fn reject_app_submission(&self, message: &str) -> Result<()> {
        self.write_tx(|conn| {

                    let tx = &mut *conn;
                    if let Some((run, step, task)) = tx
                        .query_opt(
                            "SELECT run_id,step_id,task_id FROM app_run_steps WHERE message_id=?",
                            [message],
                            |r| {
                                Ok((
                                    r.get::<_, String>(0)?,
                                    r.get::<_, String>(1)?,
                                    r.get::<_, String>(2)?,
                                ))
                            },
                        )?
                    {
                        let changed=tx.execute("UPDATE messages SET state='failed',error='app submission authorization changed',completed=? WHERE id=? AND state IN ('queued','submitting')",params![now(),message])?;
                        if changed > 0 {
                            self.app_step_failed_in(
                                &tx,
                                &run,
                                &step,
                                &task,
                                "app submission authorization changed",
                            )?;
                        }
                    }
                    Ok(())
        })
    }
}

impl Store {
    /// Transcripts can retain app material after a turn ends. Never infer
    /// public readability merely from an idle endpoint.
    #[doc(hidden)]
    pub fn app_material_endpoint(&self, alias: &str) -> Result<bool> {
        Ok(self.conn().query_row("SELECT 1 FROM app_run_steps s JOIN tasks t ON t.id=s.task_id WHERE t.assignee=? LIMIT 1", [alias], |_| Ok(())).optional()?.is_some())
    }
    #[doc(hidden)]
    pub fn app_effect_guard(&self, alias: &str, task: Option<&str>) -> Result<()> {
        if self
            .running_messages(alias)?
            .iter()
            .any(|m| m.source == "app_run_dispatch")
            || task
                .map(|id| self.app_task_owned(id))
                .transpose()?
                .unwrap_or(false)
        {
            return Err(Error::rejected("local app runs authorize run-owned text artifacts only; account grants do not authorize app outward effects"));
        }
        Ok(())
    }
}
