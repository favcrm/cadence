//! CAD-1085: durable authority for the bounded keyless native model turn.
//!
//! This module owns every row the native model broker admits, freezes or
//! settles. It mints nothing: a row is written only inside the caller's
//! [`crate::store::WriteTxn`], so the existing authority journal seals the
//! change and the caller checkpoints the committed head before any private
//! SDK child may run. There is no second ledger.
//!
//! What the tables are:
//!
//! - `native_model_sources` — the broker-private encrypted provider source
//!   (opaque account + revision, pinned origin, vault locator, key id).
//!   The secret itself lives in the encrypted custody vault, never here.
//! - `native_model_source_history` — append-only link/rotate/lock/revoke
//!   history; changing a cap never erases it.
//! - `native_model_tasks` — the durable server-owned model task bound before
//!   a Pi prompt. A duplicate durable message delivery resolves to the same
//!   task; settlement/interruption fences it for new preparation.
//! - `native_model_configurations` — the immutable frozen configuration a
//!   request was admitted under (role/model/effort/route/graph/snapshot).
//! - `native_model_requests` — prepare → committed → done/refused/unknown,
//!   the credential-free input digest, bounded usage/billing projections and
//!   the sliding-window/concurrency history.
//!
//! All counters/revisions are SQLite integers written from `u64`/`i64` that
//! the broker bounds to `i64::MAX`; canonical wire strings are produced by the
//! broker, not stored as formatted numbers.

use crate::error::{Error, Result};
use rusqlite::OptionalExtension;

use crate::store::seal::{StoreConn, WriteTxn};

/// The five tables this module contributes to schema 57. The migration
/// owner folds [`SCHEMA_V57`] into the same forward step as the protected
/// Connection schema so no store can hold one without the other.
pub(crate) const SCHEMA_V57: &str = "\
CREATE TABLE IF NOT EXISTS native_model_sources(
    source_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    account TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
    origin TEXT NOT NULL,
    origin_kind TEXT NOT NULL CHECK(origin_kind IN('devin_credentials','operator_pinned')),
    catalog_revision INTEGER NOT NULL CHECK(typeof(catalog_revision)='integer' AND catalog_revision>0),
    model_uid_map TEXT NOT NULL,
    vault_reference TEXT NOT NULL,
    key_id BLOB NOT NULL,
    state TEXT NOT NULL CHECK(state IN('active','locked','revoked')),
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS native_model_source_active
    ON native_model_sources(workspace_id,provider,account)
    WHERE state='active';
CREATE INDEX IF NOT EXISTS native_model_source_workspace
    ON native_model_sources(workspace_id,provider,state);

CREATE TABLE IF NOT EXISTS native_model_source_history(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
    action TEXT NOT NULL CHECK(action IN('linked','rotated','locked','revoked','verified')),
    actor TEXT NOT NULL,
    at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS native_model_source_history_source
    ON native_model_source_history(source_id,seq);

CREATE TABLE IF NOT EXISTS native_model_tasks(
    durable_task_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    agent_uid TEXT NOT NULL,
    session_binding TEXT NOT NULL,
    client_message_id TEXT NOT NULL,
    server_message_id TEXT NOT NULL,
    turn_token TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN('active','fenced')),
    created_ms INTEGER NOT NULL,
    fenced_ms INTEGER,
    fence_reason TEXT,
    UNIQUE(workspace_id,agent_uid,client_message_id)
);
CREATE INDEX IF NOT EXISTS native_model_task_state
    ON native_model_tasks(workspace_id,agent_uid,state);

CREATE TABLE IF NOT EXISTS native_model_configurations(
    configuration_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
    workspace_id TEXT NOT NULL,
    agent_uid TEXT NOT NULL,
    session_binding TEXT NOT NULL,
    runtime_id TEXT NOT NULL,
    runtime_generation INTEGER NOT NULL CHECK(runtime_generation>0),
    owner_epoch INTEGER NOT NULL CHECK(owner_epoch>0),
    role TEXT NOT NULL,
    model TEXT NOT NULL,
    model_uid TEXT NOT NULL,
    effort TEXT NOT NULL,
    provider TEXT NOT NULL,
    source_id TEXT NOT NULL,
    source_revision INTEGER NOT NULL CHECK(source_revision>0),
    account TEXT NOT NULL,
    origin TEXT NOT NULL,
    destination TEXT NOT NULL,
    billing_route TEXT NOT NULL,
    sdk_graph_digest TEXT NOT NULL,
    approved_tools_json TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    snapshot_expires_ms INTEGER NOT NULL CHECK(typeof(snapshot_expires_ms)='integer' AND snapshot_expires_ms>0),
    loaded_receipt_id TEXT NOT NULL,
    max_output_tokens INTEGER NOT NULL CHECK(max_output_tokens>0),
    created_ms INTEGER NOT NULL,
    UNIQUE(agent_uid,session_binding,model_uid,effort,source_id,source_revision,\
runtime_generation,owner_epoch,snapshot_id,loaded_receipt_id,max_output_tokens)
);
CREATE INDEX IF NOT EXISTS native_model_configuration_agent
    ON native_model_configurations(workspace_id,agent_uid,revision);

CREATE TABLE IF NOT EXISTS native_model_requests(
    request_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    agent_uid TEXT NOT NULL,
    durable_task_id TEXT NOT NULL,
    request_index INTEGER NOT NULL CHECK(typeof(request_index)='integer' AND request_index>=0),
    configuration_id TEXT NOT NULL,
    configuration_revision INTEGER NOT NULL CHECK(configuration_revision>0),
    snapshot_id TEXT NOT NULL,
    loaded_receipt_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    account TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN('prepared','committed','done','refused',\
'result_refused','unknown')),
    input_digest TEXT,
    assistant_json TEXT,
    usage_json TEXT,
    error_json TEXT,
    billing_json TEXT NOT NULL,
    auth_requests INTEGER NOT NULL DEFAULT 0 CHECK(auth_requests>=0),
    inference_requests INTEGER NOT NULL DEFAULT 0 CHECK(inference_requests>=0),
    created_ms INTEGER NOT NULL,
    committed_ms INTEGER,
    settled_ms INTEGER,
    UNIQUE(workspace_id,agent_uid,durable_task_id,request_index)
);
CREATE INDEX IF NOT EXISTS native_model_request_window
    ON native_model_requests(agent_uid,committed_ms);
CREATE INDEX IF NOT EXISTS native_model_request_account_window
    ON native_model_requests(workspace_id,source_id,committed_ms);
CREATE INDEX IF NOT EXISTS native_model_request_unresolved
    ON native_model_requests(state);
";

/// The request lifecycle. `prepared` is reserve-free (a selector only);
/// only `committed` consumes rate/concurrency.
pub(crate) const STATE_PREPARED: &str = "prepared";
pub(crate) const STATE_COMMITTED: &str = "committed";
pub(crate) const STATE_DONE: &str = "done";
pub(crate) const STATE_REFUSED: &str = "refused";
pub(crate) const STATE_RESULT_REFUSED: &str = "result_refused";
pub(crate) const STATE_UNKNOWN: &str = "unknown";

pub(crate) const TASK_ACTIVE: &str = "active";

pub(crate) const SOURCE_ACTIVE: &str = "active";
pub(crate) const SOURCE_LOCKED: &str = "locked";
pub(crate) const SOURCE_REVOKED: &str = "revoked";

pub(crate) const ORIGIN_CREDENTIALS: &str = "devin_credentials";
pub(crate) const ORIGIN_PINNED: &str = "operator_pinned";

fn storage(message: &str) -> Error {
    Error::invalid("storage_unavailable", message)
}

fn refused(message: &str) -> Error {
    Error::invalid("invalid_request", message)
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) struct SourceRow {
    pub source_id: String,
    pub workspace_id: String,
    pub provider: String,
    pub account: String,
    pub revision: i64,
    pub origin: String,
    pub origin_kind: String,
    pub catalog_revision: i64,
    pub model_uid_map: String,
    pub vault_reference: String,
    pub key_id: Vec<u8>,
    pub state: String,
    pub created_ms: i64,
    pub updated_ms: i64,
}

fn source_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceRow> {
    Ok(SourceRow {
        source_id: row.get("source_id")?,
        workspace_id: row.get("workspace_id")?,
        provider: row.get("provider")?,
        account: row.get("account")?,
        revision: row.get("revision")?,
        origin: row.get("origin")?,
        origin_kind: row.get("origin_kind")?,
        catalog_revision: row.get("catalog_revision")?,
        model_uid_map: row.get("model_uid_map")?,
        vault_reference: row.get("vault_reference")?,
        key_id: row.get("key_id")?,
        state: row.get("state")?,
        created_ms: row.get("created_ms")?,
        updated_ms: row.get("updated_ms")?,
    })
}

const SOURCE_COLUMNS: &str = "source_id,workspace_id,provider,account,revision,origin,\
origin_kind,catalog_revision,model_uid_map,vault_reference,key_id,state,created_ms,updated_ms";

// Nonce checks and the consuming source selection share exactly this predicate.
// The nonce path projects only 1, never copying private vault/key metadata.
const ROTATION_SOURCE_FILTER: &str = "source_id=?1 AND provider=?2 AND revision=?3 \
AND state='active' AND workspace_id=(SELECT workspace_id FROM connection_metadata WHERE singleton=1)";

pub(crate) fn rotation_source_in(
    conn: &impl StoreConn,
    source_id: &str,
    provider: &str,
    expected_revision: i64,
) -> Result<SourceRow> {
    conn.query_opt(
        &format!(
            "SELECT {SOURCE_COLUMNS} FROM native_model_sources WHERE {ROTATION_SOURCE_FILTER}"
        ),
        rusqlite::params![source_id, provider, expected_revision],
        source_from,
    )
    .map_err(|_| storage("the native model source is unavailable"))?
    .ok_or_else(|| Error::invalid("request_changed", "the native model source changed"))
}

pub(super) fn rotation_source_matches_in(
    conn: &impl StoreConn,
    source_id: &str,
    provider: &str,
    expected_revision: i64,
) -> Result<bool> {
    conn.query_opt(
        &format!("SELECT 1 FROM native_model_sources WHERE {ROTATION_SOURCE_FILTER}"),
        rusqlite::params![source_id, provider, expected_revision],
        |row| row.get::<_, i64>(0),
    )
    .map(|row| row.is_some())
    .map_err(|_| storage("the native model source is unavailable"))
}

pub(crate) fn source_in(
    conn: &impl StoreConn,
    workspace_id: &str,
    source_id: &str,
) -> Result<Option<SourceRow>> {
    conn.query_opt(
        &format!(
            "SELECT {SOURCE_COLUMNS} FROM native_model_sources \
                  WHERE workspace_id=?1 AND source_id=?2"
        ),
        rusqlite::params![workspace_id, source_id],
        source_from,
    )
    .map_err(|_| storage("the native model source is unavailable"))
}

pub(crate) fn active_source_in(
    conn: &impl StoreConn,
    workspace_id: &str,
    provider: &str,
) -> Result<Option<SourceRow>> {
    let selected: Option<Option<SourceRow>> = conn
        .query_opt(
            &format!(
                "WITH candidates AS (SELECT {SOURCE_COLUMNS} FROM native_model_sources \
             WHERE workspace_id=?1 AND provider=?2 AND state='active' LIMIT 2) \
             SELECT *,COUNT(*) OVER() AS candidate_count FROM candidates LIMIT 1"
            ),
            rusqlite::params![workspace_id, provider],
            |row| {
                if row.get::<_, i64>("candidate_count")? == 1 {
                    source_from(row).map(Some)
                } else {
                    Ok(None)
                }
            },
        )
        .map_err(|_| storage("the native model source is unavailable"))?;
    match selected {
        None => Ok(None),
        Some(Some(source)) => Ok(Some(source)),
        Some(None) => Err(Error::invalid(
            "auth_unavailable",
            "the native model sign-in is ambiguous",
        )),
    }
}

/// A fresh enrollment. The caller has already screened the secret and written
/// the encrypted vault row; this records only the metadata and its history.
pub(crate) fn insert_source_in(tx: &WriteTxn<'_>, row: &SourceRow) -> Result<()> {
    let inserted = tx
        .execute_raw(
            &format!(
                "INSERT INTO native_model_sources({SOURCE_COLUMNS}) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"
            ),
            rusqlite::params![
                row.source_id,
                row.workspace_id,
                row.provider,
                row.account,
                row.revision,
                row.origin,
                row.origin_kind,
                row.catalog_revision,
                row.model_uid_map,
                row.vault_reference,
                row.key_id,
                row.state,
                row.created_ms,
                row.updated_ms,
            ],
        )
        .map_err(|_| refused("the native model source could not be recorded"))?;
    if inserted != 1 {
        return Err(refused("the native model source could not be recorded"));
    }
    Ok(())
}

/// Rotate onto a new revision/reference. The vault row is append-only, so a
/// rotation adds a reference; the source row names the current one.
#[allow(clippy::too_many_arguments)] // the rotation mirrors the frozen source schema
pub(crate) fn rotate_source_in(
    tx: &WriteTxn<'_>,
    workspace_id: &str,
    source_id: &str,
    expected_revision: i64,
    account: &str,
    origin: &str,
    origin_kind: &str,
    catalog_revision: i64,
    model_uid_map: &str,
    vault_reference: &str,
    key_id: &[u8],
    now_ms: i64,
) -> Result<()> {
    let changed = tx
        .execute_raw(
            "UPDATE native_model_sources SET revision=revision+1,account=?4,origin=?5,\
             origin_kind=?6,catalog_revision=?7,model_uid_map=?8,vault_reference=?9,\
             key_id=?10,updated_ms=?11 \
             WHERE workspace_id=?1 AND source_id=?2 AND revision=?3 AND state='active'",
            rusqlite::params![
                workspace_id,
                source_id,
                expected_revision,
                account,
                origin,
                origin_kind,
                catalog_revision,
                model_uid_map,
                vault_reference,
                key_id,
                now_ms,
            ],
        )
        .map_err(|_| refused("the native model source could not be rotated"))?;
    if changed != 1 {
        return Err(Error::invalid(
            "stale_configuration",
            "the native model source changed",
        ));
    }
    Ok(())
}

pub(crate) fn set_source_state_in(
    tx: &WriteTxn<'_>,
    workspace_id: &str,
    source_id: &str,
    state: &str,
    now_ms: i64,
) -> Result<()> {
    let changed = tx
        .execute_raw(
            "UPDATE native_model_sources SET state=?3,updated_ms=?4 \
             WHERE workspace_id=?1 AND source_id=?2",
            rusqlite::params![workspace_id, source_id, state, now_ms],
        )
        .map_err(|_| refused("the native model source could not be updated"))?;
    if changed != 1 {
        return Err(refused("the native model source is unknown"));
    }
    Ok(())
}

pub(crate) fn source_history_in(
    tx: &WriteTxn<'_>,
    source_id: &str,
    revision: i64,
    action: &str,
    actor: &str,
    now_ms: i64,
) -> Result<()> {
    tx.execute_raw(
        "INSERT INTO native_model_source_history(source_id,revision,action,actor,at_ms) \
         VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![source_id, revision, action, actor, now_ms],
    )
    .map_err(|_| refused("the native model source history could not be recorded"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) struct TaskRow {
    pub durable_task_id: String,
    pub workspace_id: String,
    pub agent_uid: String,
    pub session_binding: String,
    pub client_message_id: String,
    pub server_message_id: String,
    pub turn_token: String,
    pub state: String,
    pub created_ms: i64,
}

fn task_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        durable_task_id: row.get("durable_task_id")?,
        workspace_id: row.get("workspace_id")?,
        agent_uid: row.get("agent_uid")?,
        session_binding: row.get("session_binding")?,
        client_message_id: row.get("client_message_id")?,
        server_message_id: row.get("server_message_id")?,
        turn_token: row.get("turn_token")?,
        state: row.get("state")?,
        created_ms: row.get("created_ms")?,
    })
}

const TASK_COLUMNS: &str = "durable_task_id,workspace_id,agent_uid,session_binding,\
client_message_id,server_message_id,turn_token,state,created_ms";

pub(crate) fn task_in(
    conn: &impl StoreConn,
    workspace_id: &str,
    agent_uid: &str,
    client_message_id: &str,
) -> Result<Option<TaskRow>> {
    conn.query_opt(
        &format!(
            "SELECT {TASK_COLUMNS} FROM native_model_tasks \
             WHERE workspace_id=?1 AND agent_uid=?2 AND client_message_id=?3"
        ),
        rusqlite::params![workspace_id, agent_uid, client_message_id],
        task_from,
    )
    .map_err(|_| storage("the native model task is unavailable"))
}

pub(crate) fn task_by_id_in(
    conn: &impl StoreConn,
    durable_task_id: &str,
) -> Result<Option<TaskRow>> {
    conn.query_opt(
        &format!("SELECT {TASK_COLUMNS} FROM native_model_tasks WHERE durable_task_id=?1"),
        [durable_task_id],
        task_from,
    )
    .map_err(|_| storage("the native model task is unavailable"))
}

/// Insert the task, or return the existing one for a duplicate durable
/// message delivery. A fenced task stays fenced: a redelivered message does
/// not reopen inference authority.
pub(crate) fn bind_task_in(tx: &WriteTxn<'_>, row: &TaskRow) -> Result<TaskRow> {
    tx.execute_raw(
        "INSERT INTO native_model_tasks(durable_task_id,workspace_id,agent_uid,\
         session_binding,client_message_id,server_message_id,turn_token,state,created_ms) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,'active',?8) \
         ON CONFLICT(workspace_id,agent_uid,client_message_id) DO NOTHING",
        rusqlite::params![
            row.durable_task_id,
            row.workspace_id,
            row.agent_uid,
            row.session_binding,
            row.client_message_id,
            row.server_message_id,
            row.turn_token,
            row.created_ms,
        ],
    )
    .map_err(|_| refused("the native model task could not be bound"))?;
    task_in(
        tx,
        &row.workspace_id,
        &row.agent_uid,
        &row.client_message_id,
    )?
    .ok_or_else(|| refused("the native model task could not be bound"))
}

pub(crate) fn fence_task_in(
    tx: &WriteTxn<'_>,
    durable_task_id: &str,
    reason: &str,
    now_ms: i64,
) -> Result<()> {
    tx.execute_raw(
        "UPDATE native_model_tasks SET state='fenced',fenced_ms=?2,fence_reason=?3 \
         WHERE durable_task_id=?1 AND state='active'",
        rusqlite::params![durable_task_id, now_ms, reason],
    )
    .map_err(|_| storage("the native model task could not be fenced"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Configurations
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigurationRow {
    pub configuration_id: String,
    pub revision: i64,
    pub workspace_id: String,
    pub agent_uid: String,
    pub session_binding: String,
    pub runtime_id: String,
    pub runtime_generation: i64,
    pub owner_epoch: i64,
    pub role: String,
    pub model: String,
    pub model_uid: String,
    pub effort: String,
    pub provider: String,
    pub source_id: String,
    pub source_revision: i64,
    pub account: String,
    pub origin: String,
    pub destination: String,
    pub billing_route: String,
    pub sdk_graph_digest: String,
    pub approved_tools_json: String,
    pub manifest_digest: String,
    pub snapshot_id: String,
    pub snapshot_expires_ms: i64,
    pub loaded_receipt_id: String,
    pub max_output_tokens: i64,
    pub created_ms: i64,
}

fn configuration_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConfigurationRow> {
    Ok(ConfigurationRow {
        configuration_id: row.get("configuration_id")?,
        revision: row.get("revision")?,
        workspace_id: row.get("workspace_id")?,
        agent_uid: row.get("agent_uid")?,
        session_binding: row.get("session_binding")?,
        runtime_id: row.get("runtime_id")?,
        runtime_generation: row.get("runtime_generation")?,
        owner_epoch: row.get("owner_epoch")?,
        role: row.get("role")?,
        model: row.get("model")?,
        model_uid: row.get("model_uid")?,
        effort: row.get("effort")?,
        provider: row.get("provider")?,
        source_id: row.get("source_id")?,
        source_revision: row.get("source_revision")?,
        account: row.get("account")?,
        origin: row.get("origin")?,
        destination: row.get("destination")?,
        billing_route: row.get("billing_route")?,
        sdk_graph_digest: row.get("sdk_graph_digest")?,
        approved_tools_json: row.get("approved_tools_json")?,
        manifest_digest: row.get("manifest_digest")?,
        snapshot_id: row.get("snapshot_id")?,
        snapshot_expires_ms: row.get("snapshot_expires_ms")?,
        loaded_receipt_id: row.get("loaded_receipt_id")?,
        max_output_tokens: row.get("max_output_tokens")?,
        created_ms: row.get("created_ms")?,
    })
}

const CONFIGURATION_COLUMNS: &str = "configuration_id,revision,workspace_id,agent_uid,\
session_binding,runtime_id,runtime_generation,owner_epoch,role,model,model_uid,effort,\
provider,source_id,source_revision,account,origin,destination,billing_route,\
sdk_graph_digest,approved_tools_json,manifest_digest,snapshot_id,snapshot_expires_ms,\
loaded_receipt_id,max_output_tokens,created_ms";

pub(crate) fn configuration_in(
    conn: &impl StoreConn,
    configuration_id: &str,
) -> Result<Option<ConfigurationRow>> {
    conn.query_opt(
        &format!(
            "SELECT {CONFIGURATION_COLUMNS} FROM native_model_configurations \
             WHERE configuration_id=?1"
        ),
        [configuration_id],
        configuration_from,
    )
    .map_err(|_| storage("the native model configuration is unavailable"))
}

/// The next revision for one agent's configuration lineage.
pub(crate) fn next_configuration_revision_in(
    conn: &impl StoreConn,
    agent_uid: &str,
) -> Result<i64> {
    let current: Option<i64> = conn
        .query_row(
            "SELECT MAX(revision) FROM native_model_configurations WHERE agent_uid=?1",
            [agent_uid],
            |row| row.get(0),
        )
        .map_err(|_| storage("the native model configuration revision is unavailable"))?;
    current
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| refused("the native model configuration revision is exhausted"))
}

/// Insert the frozen configuration, or reuse the byte-identical existing one
/// (a duplicate prepare under the same current facts).
pub(crate) fn put_configuration_in(
    tx: &WriteTxn<'_>,
    row: &ConfigurationRow,
) -> Result<ConfigurationRow> {
    tx.execute_raw(
        &format!(
            "INSERT INTO native_model_configurations({CONFIGURATION_COLUMNS}) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,\
?19,?20,?21,?22,?23,?24,?25,?26,?27) \
             ON CONFLICT(agent_uid,session_binding,model_uid,effort,source_id,\
source_revision,runtime_generation,owner_epoch,snapshot_id,loaded_receipt_id,\
max_output_tokens) DO NOTHING"
        ),
        rusqlite::params![
            row.configuration_id,
            row.revision,
            row.workspace_id,
            row.agent_uid,
            row.session_binding,
            row.runtime_id,
            row.runtime_generation,
            row.owner_epoch,
            row.role,
            row.model,
            row.model_uid,
            row.effort,
            row.provider,
            row.source_id,
            row.source_revision,
            row.account,
            row.origin,
            row.destination,
            row.billing_route,
            row.sdk_graph_digest,
            row.approved_tools_json,
            row.manifest_digest,
            row.snapshot_id,
            row.snapshot_expires_ms,
            row.loaded_receipt_id,
            row.max_output_tokens,
            row.created_ms,
        ],
    )
    .map_err(|_| refused("the native model configuration could not be frozen"))?;
    let existing: Option<ConfigurationRow> = tx
        .query_row_raw(
            &format!(
                "SELECT {CONFIGURATION_COLUMNS} FROM native_model_configurations \
                 WHERE agent_uid=?1 AND session_binding=?2 AND model_uid=?3 AND effort=?4 \
                 AND source_id=?5 AND source_revision=?6 AND runtime_generation=?7 \
                 AND owner_epoch=?8 AND snapshot_id=?9 AND loaded_receipt_id=?10 \
                 AND snapshot_expires_ms=?11 AND max_output_tokens=?12"
            ),
            rusqlite::params![
                row.agent_uid,
                row.session_binding,
                row.model_uid,
                row.effort,
                row.source_id,
                row.source_revision,
                row.runtime_generation,
                row.owner_epoch,
                row.snapshot_id,
                row.loaded_receipt_id,
                row.snapshot_expires_ms,
                row.max_output_tokens,
            ],
            configuration_from,
        )
        .optional()
        .map_err(|_| storage("the native model configuration is unavailable"))?;
    existing.ok_or_else(|| refused("the native model configuration could not be frozen"))
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) struct RequestRow {
    pub request_id: String,
    pub workspace_id: String,
    pub agent_uid: String,
    pub durable_task_id: String,
    pub request_index: i64,
    pub configuration_id: String,
    pub configuration_revision: i64,
    pub snapshot_id: String,
    pub loaded_receipt_id: String,
    pub source_id: String,
    pub account: String,
    pub state: String,
    pub input_digest: Option<String>,
    pub assistant_json: Option<String>,
    pub usage_json: Option<String>,
    pub error_json: Option<String>,
    pub billing_json: String,
    pub created_ms: i64,
}

fn request_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<RequestRow> {
    Ok(RequestRow {
        request_id: row.get("request_id")?,
        workspace_id: row.get("workspace_id")?,
        agent_uid: row.get("agent_uid")?,
        durable_task_id: row.get("durable_task_id")?,
        request_index: row.get("request_index")?,
        configuration_id: row.get("configuration_id")?,
        configuration_revision: row.get("configuration_revision")?,
        snapshot_id: row.get("snapshot_id")?,
        loaded_receipt_id: row.get("loaded_receipt_id")?,
        source_id: row.get("source_id")?,
        account: row.get("account")?,
        state: row.get("state")?,
        input_digest: row.get("input_digest")?,
        assistant_json: row.get("assistant_json")?,
        usage_json: row.get("usage_json")?,
        error_json: row.get("error_json")?,
        billing_json: row.get("billing_json")?,
        created_ms: row.get("created_ms")?,
    })
}

const REQUEST_COLUMNS: &str = "request_id,workspace_id,agent_uid,durable_task_id,\
request_index,configuration_id,configuration_revision,snapshot_id,loaded_receipt_id,\
source_id,account,state,input_digest,assistant_json,usage_json,error_json,billing_json,created_ms";

pub(crate) fn request_in(conn: &impl StoreConn, request_id: &str) -> Result<Option<RequestRow>> {
    conn.query_opt(
        &format!("SELECT {REQUEST_COLUMNS} FROM native_model_requests WHERE request_id=?1"),
        [request_id],
        request_from,
    )
    .map_err(|_| storage("the native model request is unavailable"))
}

pub(crate) fn request_by_index_in(
    conn: &impl StoreConn,
    workspace_id: &str,
    agent_uid: &str,
    durable_task_id: &str,
    request_index: i64,
) -> Result<Option<RequestRow>> {
    conn.query_opt(
        &format!(
            "SELECT {REQUEST_COLUMNS} FROM native_model_requests \
             WHERE workspace_id=?1 AND agent_uid=?2 AND durable_task_id=?3 \
             AND request_index=?4"
        ),
        rusqlite::params![workspace_id, agent_uid, durable_task_id, request_index],
        request_from,
    )
    .map_err(|_| storage("the native model request is unavailable"))
}

pub(crate) fn max_request_index_in(
    conn: &impl StoreConn,
    durable_task_id: &str,
) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT MAX(request_index) FROM native_model_requests WHERE durable_task_id=?1",
        [durable_task_id],
        |row| row.get(0),
    )
    .map_err(|_| storage("the native model request index is unavailable"))
}

pub(crate) fn insert_prepared_in(tx: &WriteTxn<'_>, row: &RequestRow) -> Result<()> {
    let inserted = tx
        .execute_raw(
            "INSERT INTO native_model_requests(request_id,workspace_id,agent_uid,\
             durable_task_id,request_index,configuration_id,configuration_revision,\
             snapshot_id,loaded_receipt_id,source_id,account,state,billing_json,created_ms) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'prepared',?12,?13)",
            rusqlite::params![
                row.request_id,
                row.workspace_id,
                row.agent_uid,
                row.durable_task_id,
                row.request_index,
                row.configuration_id,
                row.configuration_revision,
                row.snapshot_id,
                row.loaded_receipt_id,
                row.source_id,
                row.account,
                row.billing_json,
                row.created_ms,
            ],
        )
        .map_err(|_| refused("the native model request could not be prepared"))?;
    if inserted != 1 {
        return Err(refused("the native model request could not be prepared"));
    }
    Ok(())
}

/// Freeze the credential-free input binding exactly once. A second invoke for
/// the same request must present the identical digest.
pub(crate) fn freeze_input_in(
    tx: &WriteTxn<'_>,
    request_id: &str,
    input_digest: &str,
) -> Result<()> {
    tx.execute_raw(
        "UPDATE native_model_requests SET input_digest=?2 \
         WHERE request_id=?1 AND state='prepared' AND input_digest IS NULL",
        rusqlite::params![request_id, input_digest],
    )
    .map_err(|_| storage("the native model request binding could not be frozen"))?;
    Ok(())
}

/// One transaction: revalidate, reserve every dimension and commit. The
/// caller has already re-read authority inside the same transaction.
pub(crate) fn commit_request_in(tx: &WriteTxn<'_>, request_id: &str, now_ms: i64) -> Result<()> {
    let changed = tx
        .execute_raw(
            "UPDATE native_model_requests SET state='committed',committed_ms=?2 \
             WHERE request_id=?1 AND state='prepared' AND input_digest IS NOT NULL",
            rusqlite::params![request_id, now_ms],
        )
        .map_err(|_| storage("the native model request could not be committed"))?;
    if changed != 1 {
        return Err(refused("the native model request is not prepared"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // the settlement mirrors the frozen request schema
pub(crate) fn settle_request_in(
    tx: &WriteTxn<'_>,
    row: &RequestRow,
    state: &str,
    assistant_json: Option<&str>,
    usage_json: Option<&str>,
    error_json: Option<&str>,
    billing_json: &str,
    auth_requests: i64,
    inference_requests: i64,
    now_ms: i64,
) -> Result<()> {
    tx.execute_raw(
        "UPDATE native_model_requests SET state=?2,assistant_json=?3,usage_json=?4,\
         error_json=?5,billing_json=?6,auth_requests=?7,inference_requests=?8,settled_ms=?9 \
         WHERE request_id=?1 AND state IN('committed','prepared','unknown')",
        rusqlite::params![
            row.request_id,
            state,
            assistant_json,
            usage_json,
            error_json,
            billing_json,
            auth_requests,
            inference_requests,
            now_ms,
        ],
    )
    .map_err(|_| storage("the native model request could not be settled"))?;
    Ok(())
}

/// The observed private child attempt counts for an ambiguous outcome. The
/// request stays `unknown` and keeps its reserved concurrency.
pub(crate) fn note_unknown_in(
    tx: &WriteTxn<'_>,
    request_id: &str,
    auth_requests: i64,
    inference_requests: i64,
    error_json: &str,
    billing_json: &str,
    now_ms: i64,
) -> Result<()> {
    tx.execute_raw(
        "UPDATE native_model_requests SET state='unknown',auth_requests=?2,\
         inference_requests=?3,error_json=?4,billing_json=?5,committed_ms=COALESCE(committed_ms,?6) \
         WHERE request_id=?1 AND state IN('prepared','committed','unknown')",
        rusqlite::params![
            request_id,
            auth_requests,
            inference_requests,
            error_json,
            billing_json,
            now_ms,
        ],
    )
    .map_err(|_| storage("the native model request outcome could not be recorded"))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WindowUsage {
    pub agent_requests: i64,
    pub agent_unresolved: i64,
    pub account_requests: i64,
    pub account_unresolved: i64,
    pub workspace_requests: i64,
    pub workspace_unresolved: i64,
}

/// The durable sliding-window and unresolved-concurrency counts. Only a
/// committed request consumes; unknown holds concurrency; a status read
/// consumes nothing.
pub(crate) fn window_usage_in(
    conn: &impl StoreConn,
    workspace_id: &str,
    agent_uid: &str,
    source_id: &str,
    now_ms: i64,
) -> Result<WindowUsage> {
    let lower = now_ms.saturating_sub(60_000);
    let row: (i64, i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT \
               (SELECT COUNT(*) FROM native_model_requests WHERE agent_uid=?1 \
                  AND committed_ms>?4 AND committed_ms<=?5),\
               (SELECT COUNT(*) FROM native_model_requests WHERE agent_uid=?1 \
                  AND state IN('committed','unknown')),\
               (SELECT COUNT(*) FROM native_model_requests WHERE workspace_id=?2 \
                  AND source_id=?3 AND committed_ms>?4 AND committed_ms<=?5),\
               (SELECT COUNT(*) FROM native_model_requests WHERE workspace_id=?2 \
                  AND source_id=?3 AND state IN('committed','unknown')),\
               (SELECT COUNT(*) FROM native_model_requests WHERE workspace_id=?2 \
                  AND committed_ms>?4 AND committed_ms<=?5),\
               (SELECT COUNT(*) FROM native_model_requests WHERE workspace_id=?2 \
                  AND state IN('committed','unknown'))",
            rusqlite::params![agent_uid, workspace_id, source_id, lower, now_ms],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .map_err(|_| storage("the native model window is unavailable"))?;
    Ok(WindowUsage {
        agent_requests: row.0,
        agent_unresolved: row.1,
        account_requests: row.2,
        account_unresolved: row.3,
        workspace_requests: row.4,
        workspace_unresolved: row.5,
    })
}

// ---------------------------------------------------------------------------
// Durable message binding
// ---------------------------------------------------------------------------

/// The durable message row facts the broker revalidates before binding a task.
#[derive(Clone, Debug)]
pub(crate) struct DurableMessage {
    pub alias: String,
    pub state: String,
    pub turn_id: Option<String>,
}

pub(crate) fn durable_message_in(
    conn: &impl StoreConn,
    message_id: &str,
) -> Result<Option<DurableMessage>> {
    conn.query_opt(
        "SELECT alias,state,turn_id FROM messages WHERE id=?1",
        [message_id],
        |row| {
            Ok(DurableMessage {
                alias: row.get(0)?,
                state: row.get(1)?,
                turn_id: row.get(2)?,
            })
        },
    )
    .map_err(|_| storage("the durable message is unavailable"))
}

/// Agent role and configured model/effort, re-derived by stable UID on every
/// prepare and final admission. Nothing here is caller input.
#[derive(Clone, Debug)]
pub(crate) struct AgentFacts {
    pub role: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub(crate) fn agent_facts_by_uid_in(
    conn: &impl StoreConn,
    uid: &str,
) -> Result<Option<AgentFacts>> {
    conn.query_opt(
        "SELECT role,model,effort FROM agents WHERE uid=?1",
        [uid],
        |row| {
            Ok(AgentFacts {
                role: row.get(0)?,
                model: row.get(1)?,
                effort: row.get(2)?,
            })
        },
    )
    .map_err(|_| storage("the native model agent facts are unavailable"))
}

// ---------------------------------------------------------------------------
// Read wrappers on the store
// ---------------------------------------------------------------------------

/// Durable read helpers for the daemon-side broker. Every one is a plain read
/// inside the store's read transaction; none writes or caches authority.
impl crate::store::Store {
    pub(crate) fn native_model_source(
        &self,
        workspace_id: &str,
        source_id: &str,
    ) -> Result<Option<SourceRow>> {
        self.read_tx(|conn| source_in(&*conn, workspace_id, source_id))
    }

    pub(crate) fn native_model_rotation_source(
        &self,
        source_id: &str,
        provider: &str,
        expected_revision: i64,
    ) -> Result<SourceRow> {
        self.read_tx(|conn| rotation_source_in(&*conn, source_id, provider, expected_revision))
    }

    pub(crate) fn native_model_task_by_id(&self, durable_task_id: &str) -> Result<Option<TaskRow>> {
        self.read_tx(|conn| task_by_id_in(&*conn, durable_task_id))
    }

    pub(crate) fn native_model_configuration(
        &self,
        configuration_id: &str,
    ) -> Result<Option<ConfigurationRow>> {
        self.read_tx(|conn| configuration_in(&*conn, configuration_id))
    }

    pub(crate) fn native_model_request(&self, request_id: &str) -> Result<Option<RequestRow>> {
        self.read_tx(|conn| request_in(&*conn, request_id))
    }

    pub(crate) fn native_model_agent(&self, uid: &str) -> Result<Option<AgentFacts>> {
        self.read_tx(|conn| agent_facts_by_uid_in(&*conn, uid))
    }

    pub(crate) fn native_model_message(&self, message_id: &str) -> Result<Option<DurableMessage>> {
        self.read_tx(|conn| durable_message_in(&*conn, message_id))
    }
}
