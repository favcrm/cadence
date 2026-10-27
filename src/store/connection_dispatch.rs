//! Protected Connection authority and the metadata-only restore journal.
//!
//! This module is the native half of the r5 contract's *authority* layer:
//!
//! - `connection_dispatch_config`, the reviewed per-Connection operation
//!   policy, the approved manifest, the one-use human approval and the
//!   hashed model quote are the durable inputs;
//! - the shared, pure [`cadence_core::connection_dispatch`] planner decides
//!   admission from those ACTUAL rows — never from a caller assertion;
//! - one `WriteTxn` appends the reservation to the protected ledger, binds
//!   the immutable [`DispatchCommitV1`] to that real head, CAS-consumes the
//!   approval, and inserts the dispatch row; and
//! - a settlement is a separate short CAS transaction.
//!
//! Two independent guarantees live here:
//!
//! 1. **No durable row is an execution permit.** The store hands back data
//!    ([`DispatchCommitV1`] and receipts) and never a handle that a restart
//!    could recover into provider access. Only the daemon's private,
//!    in-memory, consuming permit — minted after a positively NEW commit plus
//!    an authenticated external watermark acknowledgement — can reach a
//!    provider.
//! 2. **Authorization changes are journalled.** SQL triggers queue the
//!    primary key of every row whose *authorizing* columns changed. The
//!    normal writer tail seals that queue into one aggregate protected
//!    ledger record in the same transaction, and bumps
//!    `connection_metadata.authorization_revision`, so a stale admission
//!    cannot survive a credential/grant/policy/binding change.
//!
//! Provider I/O never runs here, and no journal helper takes a lock the
//! caller did not already own.

use cadence_core::connection_dispatch::canonical::{
    exact_byte_digest, material_digest_for_domain, material_digest_value,
};
use cadence_core::connection_dispatch::dto::{
    checkpoint_matches_commit, AdmissionRequestV1, AdmissionSnapshotV1, AgentOperationV1,
    ConnectionBindingV1, ConnectionFactsV1, DispatchCommitV1, DispatchError, DispatchReceiptV1,
    DispatchStateV1, ExistingDispatchStatusV1, ExistingDispatchV1, ExposureFactsV1,
    ExposureScopeV1, FenceStateV1, HumanActionRecordV1, LoadedReceiptV1, ManifestV1, ModelQuoteV1,
    OperationKindV1, OperationPolicyV1, OwnerLimitsV1, PlanOutcomeV1, PreparedDispatchV1,
    PrincipalBindingV1, ProviderKindV1, RequesterV1, ResolvedMaterialV1, RuntimeFactsV1,
    SendApprovalV1, SettlementV1, SnapshotV1, UidGrantV1, ValidatedOutcomeV1, SNAPSHOT_LIFETIME_MS,
};
use cadence_core::connection_dispatch::ledger::{
    extend_ledger_v1, genesis_ledger_head_v1, verify_ledger_chain_v1, CheckpointAckV1,
    CheckpointV1, CustodyDomainV1, LedgerHeadV1, LedgerMutationKindV1, LedgerMutationV1,
    LedgerRecordV1,
};
use cadence_core::connection_dispatch::plan::{
    plan_connection_dispatch_v1, settle_connection_dispatch_v1,
};
use cadence_core::connection_dispatch::primitives::{
    AgentUid, Counter, Digest, EpochMs, Ident, ModelId, Revision, UsdMicros, UuidV1,
};
use cadence_core::connection_dispatch::{GrantStateV1, RoleV1};
use rusqlite::{types::ValueRef, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::sync::Arc;

use super::{Store, StoreConn, WriteTxn};
use crate::error::{Error, Result};

/// The additive schema for the native protected authority. Folded into the
/// single forward `schema_version=57` step together with the credential-entry
/// tables; every statement is idempotent so a half-applied migration
/// converges on reopen.
pub(super) const SCHEMA_V57: &str = r#"
CREATE TABLE IF NOT EXISTS connection_dispatch_config(
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN(0,1)),
 ever_enabled INTEGER NOT NULL DEFAULT 0 CHECK(ever_enabled IN(0,1)),
 fenced INTEGER NOT NULL DEFAULT 0 CHECK(fenced IN(0,1)),
 protection_required INTEGER NOT NULL DEFAULT 0 CHECK(protection_required IN(0,1)),
 limits_json TEXT NOT NULL DEFAULT '{}',
 policy_digest TEXT NOT NULL DEFAULT '',
 CHECK(enabled=0 OR ever_enabled=1));
INSERT OR IGNORE INTO connection_dispatch_config(singleton) VALUES(1);
CREATE TRIGGER IF NOT EXISTS connection_config_no_reset
 BEFORE UPDATE OF ever_enabled,protection_required ON connection_dispatch_config
 WHEN (OLD.ever_enabled=1 AND NEW.ever_enabled<>1)
   OR (OLD.protection_required=1 AND NEW.protection_required<>1)
 BEGIN SELECT RAISE(ABORT,'protected history cannot be reset'); END;
CREATE TRIGGER IF NOT EXISTS connection_config_no_delete
 BEFORE DELETE ON connection_dispatch_config
 WHEN OLD.ever_enabled=1 OR OLD.protection_required=1
 BEGIN SELECT RAISE(ABORT,'protected history cannot be deleted'); END;
CREATE TABLE IF NOT EXISTS connection_operation_policies(
 connection_id TEXT PRIMARY KEY,
 credential_revision INTEGER NOT NULL CHECK(typeof(credential_revision)='integer' AND credential_revision>0),
 policy_revision INTEGER NOT NULL CHECK(typeof(policy_revision)='integer' AND policy_revision>0),
 policy_digest TEXT NOT NULL,
 policy_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN('active','revoked')));
CREATE TABLE IF NOT EXISTS connection_manifests(
 manifest_id TEXT PRIMARY KEY,
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
 policy_digest TEXT NOT NULL,
 material_digest TEXT NOT NULL,
 manifest_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN('approved','revoked')));
CREATE TRIGGER IF NOT EXISTS connection_manifest_immutable
 BEFORE UPDATE OF manifest_id,revision,policy_digest,material_digest,manifest_json ON connection_manifests
 BEGIN SELECT RAISE(ABORT,'approved manifest material is immutable'); END;
CREATE TABLE IF NOT EXISTS connection_send_approvals(
 approval_id TEXT PRIMARY KEY,
 principal_key TEXT NOT NULL,
 connection_id TEXT NOT NULL,
 credential_revision INTEGER NOT NULL CHECK(typeof(credential_revision)='integer' AND credential_revision>0),
 authorization_revision INTEGER NOT NULL CHECK(typeof(authorization_revision)='integer' AND authorization_revision>0),
 source_id TEXT NOT NULL,
 source_digest TEXT NOT NULL,
 recipient TEXT NOT NULL,
 body BLOB NOT NULL,
 approval_json TEXT NOT NULL,
 consumed_dispatch_id TEXT UNIQUE);
CREATE TRIGGER IF NOT EXISTS connection_approval_immutable
 BEFORE UPDATE OF approval_id,principal_key,connection_id,credential_revision,
 authorization_revision,source_id,source_digest,approval_json ON connection_send_approvals
 BEGIN SELECT RAISE(ABORT,'approved material is immutable'); END;
CREATE TRIGGER IF NOT EXISTS connection_approval_once
 BEFORE UPDATE OF consumed_dispatch_id ON connection_send_approvals
 WHEN OLD.consumed_dispatch_id IS NOT NULL AND NEW.consumed_dispatch_id IS NOT OLD.consumed_dispatch_id
 BEGIN SELECT RAISE(ABORT,'approval already consumed'); END;
CREATE TABLE IF NOT EXISTS connection_model_quotes(
 quote_id TEXT PRIMARY KEY,
 connection_id TEXT NOT NULL,
 credential_revision INTEGER NOT NULL CHECK(typeof(credential_revision)='integer' AND credential_revision>0),
 authorization_revision INTEGER NOT NULL CHECK(typeof(authorization_revision)='integer' AND authorization_revision>0),
 principal_key TEXT NOT NULL,
 process_activation TEXT NOT NULL,
 quote_digest TEXT NOT NULL,
 quote_json TEXT NOT NULL,
 observed_ms INTEGER NOT NULL CHECK(typeof(observed_ms)='integer' AND observed_ms>=0),
 expires_ms INTEGER NOT NULL CHECK(typeof(expires_ms)='integer' AND expires_ms>observed_ms));
CREATE INDEX IF NOT EXISTS connection_quotes_expiry ON connection_model_quotes(expires_ms);
CREATE TABLE IF NOT EXISTS connection_snapshots(
 snapshot_id TEXT PRIMARY KEY,
 workspace_id TEXT NOT NULL,
 custody_domain TEXT NOT NULL CHECK(custody_domain IN('local','hosted')),
 agent_uid TEXT NOT NULL,
 session_binding_id TEXT NOT NULL,
 runtime_generation INTEGER NOT NULL CHECK(typeof(runtime_generation)='integer' AND runtime_generation>0),
 authorization_revision INTEGER NOT NULL CHECK(typeof(authorization_revision)='integer' AND authorization_revision>0),
 manifest_id TEXT NOT NULL,
 manifest_revision INTEGER NOT NULL CHECK(typeof(manifest_revision)='integer' AND manifest_revision>0),
 manifest_digest TEXT NOT NULL,
 policy_digest TEXT NOT NULL,
 observed_registry_digest TEXT NOT NULL,
 enrollment_id TEXT NOT NULL,
 snapshot_json TEXT NOT NULL,
 issued_ms INTEGER NOT NULL CHECK(typeof(issued_ms)='integer' AND issued_ms>=0),
 expires_ms INTEGER NOT NULL CHECK(typeof(expires_ms)='integer' AND expires_ms>issued_ms));
CREATE INDEX IF NOT EXISTS connection_snapshots_agent ON connection_snapshots(agent_uid,issued_ms);
CREATE TRIGGER IF NOT EXISTS connection_snapshot_immutable
 BEFORE UPDATE ON connection_snapshots
 BEGIN SELECT RAISE(ABORT,'issued snapshot material is immutable'); END;
CREATE TRIGGER IF NOT EXISTS connection_snapshot_no_delete
 BEFORE DELETE ON connection_snapshots
 BEGIN SELECT RAISE(ABORT,'issued snapshot history is retained'); END;
CREATE TABLE IF NOT EXISTS connection_loaded_receipts(
 loaded_receipt_id TEXT PRIMARY KEY,
 snapshot_id TEXT NOT NULL,
 workspace_id TEXT NOT NULL,
 manifest_digest TEXT NOT NULL,
 observed_registry_digest TEXT NOT NULL,
 runtime_generation INTEGER NOT NULL CHECK(typeof(runtime_generation)='integer' AND runtime_generation>0),
 enrollment_id TEXT NOT NULL,
 receipt_json TEXT NOT NULL,
 issued_ms INTEGER NOT NULL CHECK(typeof(issued_ms)='integer' AND issued_ms>=0),
 expires_ms INTEGER NOT NULL CHECK(typeof(expires_ms)='integer' AND expires_ms>issued_ms));
CREATE INDEX IF NOT EXISTS connection_loaded_receipts_snapshot
 ON connection_loaded_receipts(snapshot_id);
CREATE TRIGGER IF NOT EXISTS connection_receipt_immutable
 BEFORE UPDATE ON connection_loaded_receipts
 BEGIN SELECT RAISE(ABORT,'issued loaded receipt is immutable'); END;
CREATE TRIGGER IF NOT EXISTS connection_receipt_no_delete
 BEFORE DELETE ON connection_loaded_receipts
 BEGIN SELECT RAISE(ABORT,'issued loaded receipt history is retained'); END;
CREATE TABLE IF NOT EXISTS connection_dispatches(
 dispatch_id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL,
 workspace_id TEXT NOT NULL,
 custody_domain TEXT NOT NULL CHECK(custody_domain IN('local','hosted')),
 principal_key TEXT NOT NULL,
 agent_uid TEXT,
 connection_id TEXT NOT NULL,
 credential_revision INTEGER NOT NULL CHECK(typeof(credential_revision)='integer' AND credential_revision>0),
 authorization_revision INTEGER NOT NULL CHECK(typeof(authorization_revision)='integer' AND authorization_revision>0),
 idempotency_key TEXT NOT NULL,
 idempotency_digest TEXT NOT NULL,
 payload_digest TEXT NOT NULL,
 body_digest TEXT NOT NULL,
 destination_digest TEXT NOT NULL,
 commitment_digest TEXT NOT NULL,
 commit_json TEXT NOT NULL,
 committed_ms INTEGER NOT NULL CHECK(typeof(committed_ms)='integer' AND committed_ms>=0),
 reserved_micros INTEGER NOT NULL CHECK(typeof(reserved_micros)='integer' AND reserved_micros>=0),
 state TEXT NOT NULL CHECK(state IN('committed','done','refused','unknown','overrun')),
 actual_micros INTEGER CHECK(actual_micros IS NULL OR (typeof(actual_micros)='integer' AND actual_micros>=0)),
 usage_json TEXT,
 result_json TEXT,
 error_code TEXT,
 receipt_json TEXT,
 settled_ms INTEGER CHECK(settled_ms IS NULL OR (typeof(settled_ms)='integer' AND settled_ms>=0)),
 UNIQUE(workspace_id,custody_domain,principal_key,idempotency_key));
CREATE INDEX IF NOT EXISTS connection_dispatch_principal ON connection_dispatches(principal_key,committed_ms);
CREATE INDEX IF NOT EXISTS connection_dispatch_account ON connection_dispatches(connection_id,committed_ms);
CREATE INDEX IF NOT EXISTS connection_dispatch_workspace ON connection_dispatches(workspace_id,committed_ms);
CREATE INDEX IF NOT EXISTS connection_dispatch_time ON connection_dispatches(committed_ms);
CREATE TRIGGER IF NOT EXISTS connection_dispatch_immutable
 BEFORE UPDATE OF dispatch_id,request_id,workspace_id,custody_domain,principal_key,
 agent_uid,connection_id,credential_revision,authorization_revision,idempotency_key,
 idempotency_digest,payload_digest,body_digest,destination_digest,commitment_digest,
 commit_json,committed_ms,reserved_micros ON connection_dispatches
 BEGIN SELECT RAISE(ABORT,'dispatch binding is immutable'); END;
CREATE TRIGGER IF NOT EXISTS connection_dispatch_no_delete
 BEFORE DELETE ON connection_dispatches
 BEGIN SELECT RAISE(ABORT,'dispatch history is retained'); END;
CREATE TABLE IF NOT EXISTS connection_ledger(
 sequence INTEGER PRIMARY KEY CHECK(typeof(sequence)='integer' AND sequence>0),
 database_epoch INTEGER NOT NULL CHECK(typeof(database_epoch)='integer' AND database_epoch>0),
 digest TEXT NOT NULL UNIQUE,
 record_json TEXT NOT NULL);
CREATE TRIGGER IF NOT EXISTS connection_ledger_no_update
 BEFORE UPDATE ON connection_ledger BEGIN SELECT RAISE(ABORT,'ledger is immutable'); END;
CREATE TRIGGER IF NOT EXISTS connection_ledger_no_delete
 BEFORE DELETE ON connection_ledger BEGIN SELECT RAISE(ABORT,'ledger history is retained'); END;
CREATE TABLE IF NOT EXISTS connection_authority_changes(
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 table_name TEXT NOT NULL,
 key_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS connection_authority_rows(
 table_name TEXT NOT NULL,
 key_json TEXT NOT NULL,
 digest TEXT NOT NULL,
 PRIMARY KEY(table_name,key_json));
"#;

/// One watched table: the primary-key columns that identify a row, and the
/// *authorizing* columns whose change invalidates an admission. Execution
/// progress, timestamps and provider results are deliberately absent: a row
/// that only recorded progress must not invalidate the commitment it belongs
/// to.
struct Watch {
    table: &'static str,
    keys: &'static [&'static str],
    fields: &'static [&'static str],
}

const WATCHES: &[Watch] = &[
    Watch {
        table: "platform_credentials",
        keys: &["platform", "account"],
        fields: &[
            "connection_id",
            "credential_revision",
            "custody",
            "scopes",
            "exchange",
            "fingerprint",
        ],
    },
    Watch {
        table: "platform_grants",
        keys: &["agent", "platform", "account"],
        fields: &["agent_uid", "scopes"],
    },
    Watch {
        table: "app_grants",
        keys: &["app", "agent", "platform", "account"],
        fields: &["agent_uid", "scopes", "install_id"],
    },
    Watch {
        table: "platform_defaults",
        keys: &["project", "platform"],
        fields: &["account"],
    },
    Watch {
        table: "settings_authority",
        keys: &["id"],
        fields: &["pi", "profiles", "legacy_imported"],
    },
    Watch {
        table: "model_defaults",
        keys: &["id"],
        fields: &["document"],
    },
    Watch {
        table: "agents",
        keys: &["alias"],
        fields: &[
            "uid",
            "provider",
            "endpoint_kind",
            "role",
            "team_role",
            "session_id",
            "model",
            "params",
            "generation",
            "enabled",
            "archived_at",
        ],
    },
    Watch {
        table: "crm_smtp_links",
        keys: &["install_id", "context_id"],
        fields: &[
            "connection_id",
            "auth_revision",
            "link_revision",
            "state",
            "digest",
            "request_id",
        ],
    },
    Watch {
        table: "app_bindings",
        keys: &["id"],
        fields: &[
            "install_id",
            "context_id",
            "scope_key",
            "slot",
            "revision",
            "state",
            "config",
            "digest",
            "request_id",
        ],
    },
    Watch {
        table: "platform_effects",
        keys: &["effect_id"],
        fields: &[
            "authorization_kind",
            "decision",
            "source_name",
            "source_hash",
            "input",
            "scopes",
            "connection_operation_binding",
        ],
    },
    Watch {
        table: "connection_dispatch_config",
        keys: &["singleton"],
        fields: &[
            "enabled",
            "ever_enabled",
            "fenced",
            "protection_required",
            "limits_json",
            "policy_digest",
        ],
    },
    Watch {
        table: "connection_operation_policies",
        keys: &["connection_id"],
        fields: &[
            "credential_revision",
            "policy_revision",
            "policy_digest",
            "policy_json",
            "state",
        ],
    },
    Watch {
        table: "connection_manifests",
        keys: &["manifest_id"],
        fields: &[
            "revision",
            "policy_digest",
            "material_digest",
            "manifest_json",
            "state",
        ],
    },
    Watch {
        table: "native_model_sources",
        keys: &["workspace_id", "source_id"],
        fields: &[
            "revision",
            "state",
            "origin",
            "origin_kind",
            "account",
            "vault_reference",
            "key_id",
            "catalog_revision",
            "model_uid_map",
        ],
    },
    Watch {
        table: "native_model_source_history",
        keys: &["source_id", "seq"],
        fields: &["revision", "action"],
    },
    Watch {
        table: "native_model_tasks",
        keys: &["durable_task_id"],
        fields: &["state", "fenced_ms"],
    },
    Watch {
        table: "native_model_configurations",
        keys: &["configuration_id"],
        fields: &[
            "revision",
            "model",
            "model_uid",
            "effort",
            "source_id",
            "source_revision",
            "runtime_generation",
            "snapshot_id",
            "loaded_receipt_id",
        ],
    },
    Watch {
        table: "native_model_requests",
        keys: &["request_id"],
        fields: &[
            "state",
            "input_digest",
            "committed_ms",
            "settled_ms",
            "auth_requests",
            "inference_requests",
        ],
    },
    // Approval-stream decision rows are authority, not progress: an
    // `app_approved` record (including its `revoked` invalidation) authorizes
    // app-plan use and grant reconciliation. Only the authorizing kinds queued
    // by [`install_authorizing_event_trigger`] are journaled; this entry only
    // supplies the digest shape for them. A generic trigger on `events` would
    // journal every ordinary progress event, which is deliberately excluded.
    Watch {
        table: "events",
        keys: &["seq"],
        fields: &["alias", "kind", "payload"],
    },
];

/// Current wall-clock epoch milliseconds.
pub(crate) fn now_ms() -> Result<i64> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::internal("the system clock is ahead of the epoch"))?;
    i64::try_from(elapsed.as_millis()).map_err(|_| Error::internal("the clock is out of range"))
}

fn refused() -> Error {
    Error::invalid(
        "storage_unavailable",
        "protected authority journal is unavailable",
    )
}

fn int_refusal() -> Error {
    Error::invalid(
        "storage_unavailable",
        "protected authority state is not readable",
    )
}

/// Preserve the caller-retryable classification of a SQLite failure instead
/// of flattening every error into one opaque storage fault. A writer that
/// lost the race for the write lock is [`Error::busy`]; anything else is a
/// standing storage refusal.
fn sql_error(err: rusqlite::Error) -> Error {
    match &err {
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::DatabaseBusy
                || code.code == rusqlite::ErrorCode::DatabaseLocked =>
        {
            Error::busy_coded("store_busy", "the protected authority writer is busy")
        }
        _ => Error::invalid(
            "storage_unavailable",
            "protected authority storage refused the operation",
        ),
    }
}

fn coded(err: DispatchError) -> Error {
    Error::invalid(err.code_str(), err.message)
}

fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get(1))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    Ok(columns(conn, table)?.iter().any(|name| name == column))
}

/// Forward schema 57 for the native protected authority. Runs inside the one
/// legacy-migration transaction; request handlers never execute DDL.
pub(super) fn migrate_v60(conn: &Connection) -> Result<()> {
    for (name, definition) in [
        (
            "authorization_revision",
            "authorization_revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(authorization_revision)='integer' AND authorization_revision>0)",
        ),
        (
            "admission_clock_ms",
            "admission_clock_ms INTEGER NOT NULL DEFAULT 0 CHECK(typeof(admission_clock_ms)='integer' AND admission_clock_ms>=0)",
        ),
        (
            "database_epoch",
            "database_epoch INTEGER NOT NULL DEFAULT 1 CHECK(typeof(database_epoch)='integer' AND database_epoch>0)",
        ),
        (
            "ledger_sequence",
            "ledger_sequence INTEGER NOT NULL DEFAULT 0 CHECK(typeof(ledger_sequence)='integer' AND ledger_sequence>=0)",
        ),
        ("ledger_digest", "ledger_digest TEXT NOT NULL DEFAULT ''"),
    ] {
        if !has_column(conn, "connection_metadata", name)? {
            conn.execute_batch(&format!(
                "ALTER TABLE connection_metadata ADD COLUMN {definition}"
            ))?;
        }
    }
    for table in ["platform_grants", "app_grants"] {
        if !has_column(conn, table, "agent_uid")? {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN agent_uid TEXT"))?;
        }
    }
    if !has_column(conn, "platform_effects", "connection_operation_binding")? {
        conn.execute_batch(
            "ALTER TABLE platform_effects ADD COLUMN connection_operation_binding TEXT",
        )?;
    }
    // A half-applied step may already have created the config table without
    // `protection_required`. The new trigger definitions below reference that
    // column, so converge it BEFORE the batch; drop the two config triggers so
    // a half-applied older definition cannot silently survive.
    if table_exists(conn, "connection_dispatch_config")?
        && !has_column(conn, "connection_dispatch_config", "protection_required")?
    {
        conn.execute_batch(
            "ALTER TABLE connection_dispatch_config ADD COLUMN \
             protection_required INTEGER NOT NULL DEFAULT 0 \
             CHECK(protection_required IN(0,1))",
        )?;
    }
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS connection_config_no_reset; \
         DROP TRIGGER IF EXISTS connection_config_no_delete;",
    )?;
    conn.execute_batch(SCHEMA_V57)?;
    conn.execute_batch(super::credential_requests::SCHEMA)?;
    conn.execute_batch(super::native_model_turn::SCHEMA_V57)?;
    let epoch = Revision::new(1).map_err(|_| refused())?;
    let genesis = genesis_ledger_head_v1(epoch);
    conn.execute(
        "UPDATE connection_metadata SET ledger_digest=?1 \
         WHERE singleton=1 AND ledger_sequence=0 AND ledger_digest=''",
        [genesis.digest.as_str()],
    )?;
    for watch in WATCHES {
        // `events` is append-only authority only for the narrow decision
        // kinds; a generic watch would journal every progress event.
        if watch.table == "events" {
            validate_watch(conn, watch)?;
            continue;
        }
        install_watch(conn, watch)?;
    }
    install_authorizing_event_trigger(conn)?;
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

fn key_sql(watch: &Watch, prefix: &str) -> String {
    let mut parts = Vec::with_capacity(watch.keys.len() * 2);
    for key in watch.keys {
        parts.push(format!("'{key}'"));
        parts.push(format!("{prefix}.{key}"));
    }
    format!("json_object({})", parts.join(","))
}

fn validate_watch(conn: &Connection, watch: &Watch) -> Result<()> {
    for field in watch.keys.iter().chain(watch.fields) {
        if !has_column(conn, watch.table, field)? {
            return Err(refused());
        }
    }
    Ok(())
}

fn install_watch(conn: &Connection, watch: &Watch) -> Result<()> {
    validate_watch(conn, watch)?;
    // Acceptance is gated by the durable selection of protected authority,
    // NOT by execution enablement: first-secret/native-source enrollment and
    // rotation are authorized before dispatch is ever enabled. The selection
    // is monotone and is written only by the authenticated protected boot
    // path (`install_protected_checkpoint`), so an ordinary store keeps its
    // unchanged unprotected behavior.
    let enabled = "EXISTS(SELECT 1 FROM connection_dispatch_config WHERE singleton=1 \
                   AND (ever_enabled=1 OR protection_required=1))";
    // Derived model lifecycle rows still enter the journal and require the
    // owner checkpoint. They do not change the permissions frozen in an issued
    // snapshot: final model admission checks their current state separately.
    // Every source, settings, grant, policy and key input still invalidates it.
    let invalidate_snapshot = match watch.table {
        "native_model_tasks" | "native_model_configurations" | "native_model_requests" => "",
        _ => "UPDATE connection_metadata SET authorization_revision=authorization_revision+1 WHERE singleton=1;",
    };
    let changed = watch
        .keys
        .iter()
        .chain(watch.fields)
        .map(|field| format!("OLD.{field} IS NOT NEW.{field}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    for (event, prefix) in [("INSERT", "NEW"), ("DELETE", "OLD"), ("UPDATE", "NEW")] {
        let condition = if event == "UPDATE" {
            format!("{enabled} AND ({changed})")
        } else {
            enabled.to_owned()
        };
        let old_key = if event == "UPDATE" {
            format!(
                "INSERT INTO connection_authority_changes(table_name,key_json) VALUES('{}',{});",
                watch.table,
                key_sql(watch, "OLD")
            )
        } else {
            String::new()
        };
        // Drop-then-create so a half-applied earlier definition (same trigger
        // name, different gate) converges instead of being silently kept.
        conn.execute_batch(&format!(
            "DROP TRIGGER IF EXISTS cad1085_{table}_{event}; \
             CREATE TRIGGER cad1085_{table}_{event} AFTER {event} ON {table} WHEN {condition} BEGIN \
             {old_key} INSERT INTO connection_authority_changes(table_name,key_json) VALUES('{table}',{key}); \
             {invalidate_snapshot} END;",
            table = watch.table,
            event = event.to_ascii_lowercase(),
            condition = condition,
            old_key = old_key,
            key = key_sql(watch, prefix),
            invalidate_snapshot = invalidate_snapshot,
        ))?;
    }
    Ok(())
}

/// The ONE narrow event trigger: an approval-stream `app_approved` row (the
/// approval itself and its `revoked` invalidation) authorizes app-plan use and
/// grant reconciliation. Every other event kind is ordinary progress and stays
/// out of the restore journal.
fn install_authorizing_event_trigger(conn: &Connection) -> Result<()> {
    let enabled = "EXISTS(SELECT 1 FROM connection_dispatch_config WHERE singleton=1 \
                   AND (ever_enabled=1 OR protection_required=1))";
    conn.execute_batch(&format!(
        "DROP TRIGGER IF EXISTS cad1085_events_authorizing_insert; \
         CREATE TRIGGER cad1085_events_authorizing_insert AFTER INSERT ON events \
         WHEN NEW.alias='{stream}' AND NEW.kind='{kind}' AND {enabled} BEGIN \
         INSERT INTO connection_authority_changes(table_name,key_json) \
           VALUES('events',json_object('seq',NEW.seq)); \
         UPDATE connection_metadata SET authorization_revision=authorization_revision+1 WHERE singleton=1; END;",
        stream = super::events::APPROVAL_STREAM,
        kind = super::events::APP_APPROVED_EVENT,
    ))?;
    Ok(())
}

/// An already pending queue cannot be a prior crashed Store transaction:
/// queue, digest shadows and the ledger record commit atomically. Never bless
/// a foreign partial write — refuse it and let the caller reconcile.
pub(super) fn require_clean_journal_in(tx: &WriteTxn<'_>) -> Result<()> {
    let dirty = tx
        .query_row_raw(
            "SELECT EXISTS(SELECT 1 FROM connection_authority_changes)",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(sql_error)?;
    if dirty != 0 {
        return Err(Error::invalid(
            "storage_unavailable",
            "the protected authority journal has an unsealed queue",
        ));
    }
    Ok(())
}

pub(super) fn ledger_head_in(conn: &impl StoreConn) -> Result<LedgerHeadV1> {
    let (sequence, epoch, digest): (i64, i64, String) = conn
        .query_row(
            "SELECT ledger_sequence,database_epoch,ledger_digest FROM connection_metadata WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sql_error)?;
    if sequence < 0 || epoch <= 0 {
        return Err(int_refusal());
    }
    Ok(LedgerHeadV1 {
        sequence: Counter::new(sequence as u64).map_err(|_| int_refusal())?,
        database_epoch: Revision::new(epoch as u64).map_err(|_| int_refusal())?,
        digest: Digest::try_from(digest).map_err(|_| int_refusal())?,
    })
}

fn field_value(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Value> {
    Ok(match row.get_ref(index)? {
        ValueRef::Null => json!({"type":"null"}),
        ValueRef::Integer(value) => json!({"type":"integer","value":value}),
        // `f64::to_bits` keeps the exact durable value without a decimal
        // formatting dependency or a NaN/Infinity JSON edge.
        ValueRef::Real(value) => json!({"type":"real","bits":format!("{:016x}", value.to_bits())}),
        ValueRef::Text(bytes) => json!({"type":"text","digest":exact_byte_digest(bytes)}),
        ValueRef::Blob(bytes) => json!({"type":"blob","digest":exact_byte_digest(bytes)}),
    })
}

/// Hash exactly the watched authorizing columns of one row. Text and blob
/// values are hashed in place through their borrowed bytes, so a large
/// profile document is never copied just to journal it.
fn row_digest(
    conn: &impl StoreConn,
    watch: &Watch,
    key: &Map<String, Value>,
) -> Result<Option<String>> {
    if key.len() != watch.keys.len() {
        return Err(int_refusal());
    }
    let mut values = Vec::with_capacity(watch.keys.len());
    for column in watch.keys {
        let value = key.get(*column).ok_or_else(int_refusal)?;
        values.push(match value {
            Value::String(value) => rusqlite::types::Value::Text(value.clone()),
            Value::Number(value) => {
                rusqlite::types::Value::Integer(value.as_i64().ok_or_else(int_refusal)?)
            }
            _ => return Err(int_refusal()),
        });
    }
    let predicate = watch
        .keys
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{column}=?{}", index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    // Only the authorizing columns are selected: an execution-progress write
    // must never change the digest it inherits from its own commitment.
    let select = watch.fields.join(",");
    let sql = format!("SELECT {select} FROM {} WHERE {predicate}", watch.table);
    let result = conn
        .query_opt(&sql, rusqlite::params_from_iter(values), |row| {
            let mut fields = Map::new();
            for (index, name) in watch.fields.iter().enumerate() {
                fields.insert((*name).to_owned(), field_value(row, index)?);
            }
            Ok(material_digest_for_domain(
                "cadence-protected-authority-row-v1",
                &Value::Object(fields),
            ))
        })
        .map_err(sql_error)?;
    Ok(result)
}

fn append_record_in(
    tx: &WriteTxn<'_>,
    head: &LedgerHeadV1,
    kind: LedgerMutationKindV1,
    refs: Vec<Digest>,
) -> Result<LedgerHeadV1> {
    let mutation = LedgerMutationV1 {
        v: 1,
        database_epoch: head.database_epoch,
        kind,
        refs,
    };
    let next = extend_ledger_v1(head, &mutation).map_err(|_| refused())?;
    let record = LedgerRecordV1 {
        v: 1,
        sequence: next.sequence,
        database_epoch: next.database_epoch,
        previous_digest: head.digest.clone(),
        kind: mutation.kind,
        refs: mutation.refs,
        digest: next.digest.clone(),
    };
    tx.execute_raw(
        "INSERT INTO connection_ledger(sequence,database_epoch,digest,record_json) VALUES(?1,?2,?3,?4)",
        rusqlite::params![
            record.sequence.get(),
            record.database_epoch.get(),
            record.digest.as_str(),
            serde_json::to_string(&record).map_err(|_| refused())?,
        ],
    )
    .map_err(sql_error)?;
    tx.execute_raw(
        "UPDATE connection_metadata SET ledger_sequence=?1,ledger_digest=?2 WHERE singleton=1",
        rusqlite::params![next.sequence.get(), next.digest.as_str()],
    )
    .map_err(sql_error)?;
    Ok(next)
}

/// Seal the queued authority changes into ONE aggregate ledger record before
/// the same writer commits. One aggregate ref keeps a checkpoint bounded no
/// matter how many rows changed; the per-row digest shadows stay durable in
/// `connection_authority_rows` so the next change still binds before/after.
pub(super) fn seal_authority_changes_in(tx: &WriteTxn<'_>) -> Result<LedgerHeadV1> {
    let changed: Vec<(String, String)> = tx.query_map(
        "SELECT DISTINCT table_name,key_json FROM connection_authority_changes \
         ORDER BY table_name,key_json",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut head = ledger_head_in(tx)?;
    if changed.is_empty() {
        return Ok(head);
    }
    let mut per_row = Vec::with_capacity(changed.len());
    for (table, key_json) in changed {
        let watch = WATCHES
            .iter()
            .find(|watch| watch.table == table)
            .ok_or_else(refused)?;
        let key: Map<String, Value> = match serde_json::from_str::<Value>(&key_json) {
            Ok(Value::Object(key)) => key,
            _ => return Err(refused()),
        };
        let before: Option<String> = tx
            .query_row_raw(
                "SELECT digest FROM connection_authority_rows WHERE table_name=?1 AND key_json=?2",
                rusqlite::params![table, key_json],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        let after = row_digest(tx, watch, &key)?;
        let digest = material_digest_for_domain(
            "cadence-protected-authority-change-v1",
            &json!({"table":table,"key":Value::Object(key),"before":before,"after":after}),
        );
        per_row.push(Digest::try_from(digest).map_err(|_| refused())?);
        match after {
            Some(digest) => {
                tx.execute_raw(
                    "INSERT INTO connection_authority_rows(table_name,key_json,digest) VALUES(?1,?2,?3) \
                     ON CONFLICT(table_name,key_json) DO UPDATE SET digest=excluded.digest",
                    rusqlite::params![table, key_json, digest],
                )
                .map_err(sql_error)?;
            }
            None => {
                tx.execute_raw(
                    "DELETE FROM connection_authority_rows WHERE table_name=?1 AND key_json=?2",
                    rusqlite::params![table, key_json],
                )
                .map_err(sql_error)?;
            }
        }
    }
    // Sorted inputs make the aggregate deterministic for the same batch.
    let aggregate = Digest::try_from(material_digest_for_domain(
        "cadence-protected-authority-batch-v1",
        &json!({"v":1,"rows":per_row}),
    ))
    .map_err(|_| refused())?;
    head = append_record_in(
        tx,
        &head,
        LedgerMutationKindV1::Authorization,
        vec![aggregate],
    )?;
    tx.execute_raw("DELETE FROM connection_authority_changes", [])
        .map_err(sql_error)?;
    Ok(head)
}

// ---------------------------------------------------------------------------
// Durable read helpers (actual rows only)
// ---------------------------------------------------------------------------

/// The single authoritative protected state row.
#[derive(Clone, Debug)]
pub(crate) struct Authority {
    pub workspace_id: String,
    pub database_epoch: Revision,
    pub ledger_head: LedgerHeadV1,
    pub authorization_revision: Revision,
    pub clock_floor_ms: EpochMs,
    pub enabled: bool,
    pub ever_enabled: bool,
    pub fenced: bool,
}

fn flag(value: i64) -> Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(int_refusal()),
    }
}

/// The durable selection of protected authority journaling, independent of
/// execution enablement. Only the authenticated protected boot path writes it
/// (`install_protected_checkpoint`); it is monotone. A missing table/row means
/// the ordinary unprotected store.
pub(super) fn protection_required_in(conn: &impl StoreConn) -> Result<bool> {
    let value: Option<i64> = conn
        .query_opt(
            "SELECT protection_required FROM connection_dispatch_config WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    match value {
        None => Ok(false),
        Some(value) => flag(value),
    }
}

pub(crate) fn authority_in(conn: &impl StoreConn) -> Result<Authority> {
    let (
        workspace_id,
        authorization_revision,
        clock_floor_ms,
        database_epoch,
        ledger_sequence,
        ledger_digest,
    ): (String, i64, i64, i64, i64, String) = conn
        .query_row(
            "SELECT workspace_id,authorization_revision,admission_clock_ms,\
             database_epoch,ledger_sequence,ledger_digest FROM connection_metadata WHERE singleton=1",
            [],
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
        .map_err(sql_error)?;
    let (enabled, ever_enabled, fenced): (i64, i64, i64) = conn
        .query_row(
            "SELECT enabled,ever_enabled,fenced FROM connection_dispatch_config WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sql_error)?;
    if workspace_id.is_empty()
        || authorization_revision <= 0
        || clock_floor_ms < 0
        || database_epoch <= 0
        || ledger_sequence < 0
    {
        return Err(int_refusal());
    }
    Ok(Authority {
        workspace_id,
        database_epoch: Revision::new(database_epoch as u64).map_err(|_| int_refusal())?,
        ledger_head: LedgerHeadV1 {
            sequence: Counter::new(ledger_sequence as u64).map_err(|_| int_refusal())?,
            database_epoch: Revision::new(database_epoch as u64).map_err(|_| int_refusal())?,
            digest: Digest::try_from(ledger_digest).map_err(|_| int_refusal())?,
        },
        authorization_revision: Revision::new(authorization_revision as u64)
            .map_err(|_| int_refusal())?,
        clock_floor_ms: EpochMs::new(clock_floor_ms as u64).map_err(|_| int_refusal())?,
        enabled: flag(enabled)?,
        ever_enabled: flag(ever_enabled)?,
        fenced: flag(fenced)?,
    })
}

pub(crate) fn config_limits_in(conn: &impl StoreConn) -> Result<OwnerLimitsV1> {
    let raw: String = conn
        .query_row(
            "SELECT limits_json FROM connection_dispatch_config WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    serde_json::from_str::<OwnerLimitsV1>(&raw).map_err(|_| {
        Error::invalid(
            "protected_limits_invalid",
            "the protected owner limits are absent or not finite",
        )
    })
}

pub(crate) struct ConnectionRow {
    pub connection_id: String,
    pub account: String,
    pub platform: String,
    pub credential_revision: i64,
    pub custody: String,
    pub exchange: String,
}

pub(crate) fn connection_row_in(
    conn: &impl StoreConn,
    connection_id: &str,
) -> Result<Option<ConnectionRow>> {
    conn.query_opt(
        "SELECT connection_id,account,platform,credential_revision,custody,exchange \
         FROM platform_credentials WHERE connection_id=?1",
        [connection_id],
        |row| {
            Ok(ConnectionRow {
                connection_id: row.get(0)?,
                account: row.get(1)?,
                platform: row.get(2)?,
                credential_revision: row.get(3)?,
                custody: row.get(4)?,
                exchange: row.get(5)?,
            })
        },
    )
    .map_err(sql_error)
}

pub(crate) struct PolicyRow {
    pub connection_id: String,
    pub credential_revision: i64,
    pub policy_revision: i64,
    pub policy_digest: String,
    pub policy: OperationPolicyV1,
    pub state: String,
}

pub(crate) fn policy_row_in(
    conn: &impl StoreConn,
    connection_id: &str,
) -> Result<Option<PolicyRow>> {
    let raw: Option<(String, i64, i64, String, String, String)> = conn
        .query_opt(
            "SELECT connection_id,credential_revision,policy_revision,policy_digest,policy_json,state \
             FROM connection_operation_policies WHERE connection_id=?1",
            [connection_id],
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
        .map_err(sql_error)?;
    let Some((connection_id, credential_revision, policy_revision, policy_digest, raw, state)) =
        raw
    else {
        return Ok(None);
    };
    let policy = serde_json::from_str::<OperationPolicyV1>(&raw).map_err(|_| int_refusal())?;
    Ok(Some(PolicyRow {
        connection_id,
        credential_revision,
        policy_revision,
        policy_digest,
        policy,
        state,
    }))
}

/// Stamped grants for exactly one stable agent UID bound to this exact
/// Connection account. An alias, a legacy NULL-UID row or another account's
/// grant is never returned, so it can never authorize protected use.
pub(crate) fn grants_in(
    conn: &impl StoreConn,
    agent_uid: &str,
    platform: &str,
    account: &str,
) -> Result<Vec<UidGrantV1>> {
    let parsed_uid = AgentUid::try_from(agent_uid.to_string()).map_err(|_| int_refusal())?;
    let rows: Vec<(String, String)> = conn
        .query_vec(
            "SELECT g.agent_uid,COALESCE(a.role,'worker') FROM platform_grants g \
             LEFT JOIN agents a ON a.alias=g.agent \
             WHERE g.agent_uid=?1 AND g.platform=?2 AND g.account=?3 \
             ORDER BY g.agent",
            rusqlite::params![agent_uid, platform, account],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sql_error)?;
    let mut grants = Vec::with_capacity(rows.len());
    for (uid, role) in rows {
        if uid != agent_uid {
            continue;
        }
        grants.push(UidGrantV1 {
            agent_uid: parsed_uid.clone(),
            role: if role == "master" {
                RoleV1::Master
            } else {
                RoleV1::Worker
            },
            state: GrantStateV1::Active,
            allowed_subject_ids: Vec::new(),
        });
    }
    Ok(grants)
}

/// The current Pi role/model allowlist read from the durable settings
/// authority, converted to the shared policy shape. Absent policy stays
/// absent (nothing allowed).
pub(crate) fn role_model_policy_in(
    conn: &impl StoreConn,
) -> Result<Option<cadence_core::connection_dispatch::RoleModelPolicyV1>> {
    let raw: Option<Option<String>> = conn
        .query_row("SELECT pi FROM settings_authority WHERE id=1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(sql_error)?;
    let Some(Some(raw)) = raw else {
        return Ok(None);
    };
    let policy =
        serde_json::from_str::<crate::pi_policy::PiPolicy>(&raw).map_err(|_| int_refusal())?;
    let models = |values: &[String]| -> Result<Vec<ModelId>> {
        values
            .iter()
            .map(|value| ModelId::try_from(value.clone()).map_err(|_| int_refusal()))
            .collect()
    };
    let master_allow = match &policy.models.master_allow {
        Some(values) => Some(models(values)?),
        None => None,
    };
    let worker_allow = match &policy.models.worker_allow {
        Some(values) => Some(models(values)?),
        None => None,
    };
    let agentic_providers = policy
        .agentic_providers
        .iter()
        .map(|value| Ident::try_from(value.clone()).map_err(|_| int_refusal()))
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(cadence_core::connection_dispatch::RoleModelPolicyV1 {
        allow: models(&policy.models.allow)?,
        master_allow,
        worker_allow,
        agentic_providers,
    }))
}

/// An ID allowlist cannot prove its configured provider: settings may reassign
/// the same ID without changing scope. Missing/ambiguous metadata is not
/// permission to select a native route.
pub(crate) fn require_model_provider_in(
    conn: &impl StoreConn,
    model_id: &str,
    expected_provider: &str,
) -> Result<()> {
    let (count, provider): (i64, Option<String>) = conn
        .query_row(
            "SELECT count(*),min(json_extract(m.value,'$.provider')) \
             FROM model_defaults d,json_each(d.document,'$.agent_settings.models') m \
             WHERE d.id=1 AND json_extract(m.value,'$.id')=?1",
            [model_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sql_error)?;
    if count != 1 || provider.as_deref() != Some(expected_provider) {
        return Err(Error::invalid(
            "operation_not_granted",
            "the configured model provider does not match the native route",
        ));
    }
    Ok(())
}

fn exposure_scope(
    conn: &impl StoreConn,
    filter: &str,
    key: &str,
    now_ms: u64,
) -> Result<ExposureScopeV1> {
    let lower = now_ms.saturating_sub(60_000) as i64;
    let daily_lower = now_ms.saturating_sub(86_400_000) as i64;
    let now = now_ms as i64;
    let recent: Vec<i64> = conn
        .query_vec(
            &format!(
                "SELECT committed_ms FROM connection_dispatches WHERE {filter}=?1 \
                 AND committed_ms>?2 AND committed_ms<=?3 ORDER BY committed_ms LIMIT 4096"
            ),
            rusqlite::params![key, lower, now],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let (unresolved, daily, held, lifetime): (i64, i64, i64, i64) = conn
        .query_row(
            &format!(
                "SELECT \
                   (SELECT COUNT(*) FROM connection_dispatches WHERE {filter}=?1 \
                      AND state IN('committed','unknown')),\
                   (SELECT COALESCE(SUM(actual_micros),0) FROM connection_dispatches WHERE {filter}=?1 \
                      AND state IN('done','refused','overrun') AND settled_ms>?2),\
                   (SELECT COALESCE(SUM(reserved_micros),0) FROM connection_dispatches WHERE {filter}=?1 \
                      AND state IN('committed','unknown')),\
                   (SELECT COALESCE(SUM(actual_micros),0) FROM connection_dispatches WHERE {filter}=?1 \
                      AND state IN('done','refused','overrun'))"
            ),
            rusqlite::params![key, daily_lower],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(sql_error)?;
    if unresolved < 0 || daily < 0 || held < 0 || lifetime < 0 {
        return Err(int_refusal());
    }
    let mut recent_request_ms = Vec::with_capacity(recent.len());
    for value in recent {
        if value < 0 {
            return Err(int_refusal());
        }
        recent_request_ms.push(EpochMs::new(value as u64).map_err(|_| int_refusal())?);
    }
    Ok(ExposureScopeV1 {
        recent_request_ms,
        unresolved: Counter::new(unresolved as u64).map_err(|_| int_refusal())?,
        daily_settled_micros: UsdMicros::new(daily as u64).map_err(|_| int_refusal())?,
        unresolved_reserved_micros: UsdMicros::new(held as u64).map_err(|_| int_refusal())?,
        lifetime_settled_micros: UsdMicros::new(lifetime as u64).map_err(|_| int_refusal())?,
    })
}

pub(crate) fn exposure_in(
    conn: &impl StoreConn,
    principal_key: &str,
    account_key: &str,
    workspace_id: &str,
    now_ms: u64,
) -> Result<ExposureFactsV1> {
    Ok(ExposureFactsV1 {
        principal: exposure_scope(conn, "principal_key", principal_key, now_ms)?,
        account: exposure_scope(conn, "connection_id", account_key, now_ms)?,
        workspace: exposure_scope(conn, "workspace_id", workspace_id, now_ms)?,
    })
}

pub(crate) fn manifest_row_in(
    conn: &impl StoreConn,
    manifest_id: &str,
) -> Result<Option<(ManifestV1, String)>> {
    let row: Option<(String, String)> = conn
        .query_opt(
            "SELECT manifest_json,state FROM connection_manifests WHERE manifest_id=?1",
            [manifest_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sql_error)?;
    let Some((raw, state)) = row else {
        return Ok(None);
    };
    let manifest = serde_json::from_str::<ManifestV1>(&raw).map_err(|_| int_refusal())?;
    Ok(Some((manifest, state)))
}

/// The single approved manifest bound to a reviewed policy digest. Used by
/// the human-verification path, which names no agent capability: the
/// Connection's own approved manifest is discovered from the policy, never
/// chosen by the caller.
fn approved_manifest_id_in(conn: &impl StoreConn, policy_digest: &str) -> Result<Ident> {
    let rows: Vec<String> = conn
        .query_vec(
            "SELECT manifest_id FROM connection_manifests \
             WHERE state='approved' AND policy_digest=?1 ORDER BY manifest_id",
            [policy_digest],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    match rows.as_slice() {
        [only] => ident(only),
        _ => Err(Error::invalid(
            "capability_unavailable",
            "the capability is not available",
        )),
    }
}

pub(crate) fn approval_row_in(
    conn: &impl StoreConn,
    approval_id: &str,
) -> Result<Option<(SendApprovalV1, Option<String>)>> {
    let row: Option<(String, Option<String>)> = conn
        .query_opt(
            "SELECT approval_json,consumed_dispatch_id FROM connection_send_approvals WHERE approval_id=?1",
            [approval_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sql_error)?;
    let Some((raw, consumed)) = row else {
        return Ok(None);
    };
    let approval = serde_json::from_str::<SendApprovalV1>(&raw).map_err(|_| int_refusal())?;
    Ok(Some((approval, consumed)))
}

pub(crate) fn quote_row_in(conn: &impl StoreConn, quote_id: &str) -> Result<Option<ModelQuoteV1>> {
    let raw: Option<String> = conn
        .query_opt(
            "SELECT quote_json FROM connection_model_quotes WHERE quote_id=?1",
            [quote_id],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    match raw {
        None => Ok(None),
        Some(raw) => serde_json::from_str::<ModelQuoteV1>(&raw)
            .map(Some)
            .map_err(|_| int_refusal()),
    }
}

pub(crate) fn existing_dispatch_in(
    conn: &impl StoreConn,
    workspace_id: &str,
    custody_domain: &str,
    principal_key: &str,
    idempotency_key: &str,
) -> Result<Option<ExistingDispatchV1>> {
    let raw: Option<(String, String, String, String, String, String, i64)> = conn
        .query_opt(
            "SELECT dispatch_id,request_id,state,idempotency_digest,commitment_digest,\
             payload_digest,reserved_micros FROM connection_dispatches \
             WHERE workspace_id=?1 AND custody_domain=?2 AND principal_key=?3 AND idempotency_key=?4",
            rusqlite::params![workspace_id, custody_domain, principal_key, idempotency_key],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .map_err(sql_error)?;
    let Some((dispatch, request, state, idempotency, commitment, payload, reserved)) = raw else {
        return Ok(None);
    };
    if reserved < 0 {
        return Err(int_refusal());
    }
    Ok(Some(ExistingDispatchV1 {
        v: 1,
        dispatch_id: UuidV1::try_from(dispatch).map_err(|_| int_refusal())?,
        request_id: UuidV1::try_from(request).map_err(|_| int_refusal())?,
        state: parse_state(&state)?,
        idempotency_digest: Digest::try_from(idempotency).map_err(|_| int_refusal())?,
        commitment_digest: Digest::try_from(commitment).map_err(|_| int_refusal())?,
        payload_digest: Digest::try_from(payload).map_err(|_| int_refusal())?,
        reserved_micros: UsdMicros::new(reserved as u64).map_err(|_| int_refusal())?,
    }))
}

fn parse_state(value: &str) -> Result<DispatchStateV1> {
    match value {
        "committed" => Ok(DispatchStateV1::Committed),
        "done" => Ok(DispatchStateV1::Done),
        "refused" => Ok(DispatchStateV1::Refused),
        "unknown" => Ok(DispatchStateV1::Unknown),
        "overrun" => Ok(DispatchStateV1::Overrun),
        _ => Err(int_refusal()),
    }
}

fn state_str(state: DispatchStateV1) -> &'static str {
    match state {
        DispatchStateV1::Committed => "committed",
        DispatchStateV1::Done => "done",
        DispatchStateV1::Refused => "refused",
        DispatchStateV1::Unknown => "unknown",
        DispatchStateV1::Overrun => "overrun",
    }
}

// ---------------------------------------------------------------------------
// The one admission/commit transaction
// ---------------------------------------------------------------------------

/// Trusted-adapter admission facts: what only the daemon can observe. Every
/// other input is read from the actual rows inside the commit transaction.
pub(crate) struct AdmissionFactsV1 {
    pub dispatch_id: UuidV1,
    pub request_id: UuidV1,
    pub now_ms: EpochMs,
    pub requester: RequesterV1,
    pub principal: PrincipalBindingV1,
    pub runtime: RuntimeFactsV1,
    pub resolved: ResolvedMaterialV1,
    /// The approved manifest the agent capability came from. `None` means
    /// "the one approved manifest bound to this Connection's reviewed policy"
    /// — the human-verification path, which has no agent capability.
    pub manifest_id: Option<Ident>,
    pub key_id: Ident,
    pub approval_id: Option<Ident>,
    /// The exact issued snapshot/receipt pair an agent request named. The
    /// final transaction loads these ids; it never mints a replacement.
    pub loaded_snapshot: Option<LoadedSnapshot>,
}

/// The result of the authoritative commit transaction. `commit` is data; the
/// caller's permission to execute comes only from its own in-memory permit.
#[allow(clippy::large_enum_variant)] // one commit outcome per dispatch, matched once
pub(crate) enum CommitOutcome {
    Committed(DispatchCommitV1),
    Duplicate(ExistingDispatchStatusV1),
}

fn provider_kind_of(platform: &str) -> Result<ProviderKindV1> {
    match platform {
        "smtp" => Ok(ProviderKindV1::Smtp),
        "openrouter" => Ok(ProviderKindV1::Openrouter),
        _ => Err(Error::invalid(
            "capability_unavailable",
            "the Connection does not use a reviewed protected provider",
        )),
    }
}

fn ident(value: &str) -> Result<Ident> {
    Ident::try_from(value.to_string()).map_err(|_| int_refusal())
}

fn random_uuid() -> Result<UuidV1> {
    UuidV1::try_from(uuid::Uuid::new_v4().to_string()).map_err(|_| int_refusal())
}

/// Build the admission snapshot from actual durable rows plus the trusted
/// adapter facts. Runs inside the caller's transaction.
fn snapshot_in(
    tx: &WriteTxn<'_>,
    facts: &AdmissionFactsV1,
    request: &AdmissionRequestV1,
) -> Result<(AdmissionSnapshotV1, Option<ModelQuoteV1>)> {
    let authority = authority_in(tx)?;
    if !authority.enabled || !authority.ever_enabled || authority.fenced {
        return Err(Error::invalid(
            "runtime_unqualified",
            "protected Connections are not enabled on this store",
        ));
    }
    if facts.now_ms.get() < authority.clock_floor_ms.get() {
        return Err(Error::invalid(
            "clock_regression",
            "the durable admission clock floor is ahead of the request",
        ));
    }
    // The principal binding must describe THIS workspace and the native
    // custody domain. A trusted adapter cannot repoint it at a foreign store.
    if facts.principal.workspace_id.as_str() != authority.workspace_id.as_str()
        || facts.principal.custody_domain.as_str() != "local"
        || facts.requester.agent_uid != facts.principal.agent_uid
    {
        return Err(Error::invalid(
            "principal_required",
            "a positively identified caller is required",
        ));
    }
    let connection_id = request.connection_id().as_str();
    let connection = connection_row_in(tx, connection_id)?
        .ok_or_else(|| Error::invalid("not_found", "the selector is unavailable"))?;
    if connection.connection_id != connection_id {
        return Err(Error::invalid("not_found", "the selector is unavailable"));
    }
    let provider_kind = provider_kind_of(&connection.platform)?;
    let policy_row = policy_row_in(tx, connection_id)?.ok_or_else(|| {
        Error::invalid(
            "operation_not_granted",
            "the Connection has no reviewed protected policy",
        )
    })?;
    if policy_row.connection_id != connection_id
        || policy_row.credential_revision != connection.credential_revision
        || policy_row.policy.policy_revision.get() != policy_row.policy_revision as u64
    {
        return Err(Error::invalid(
            "stale_revision",
            "the credential, authorization or policy revision changed",
        ));
    }
    if policy_row.state != "active" || !policy_row.policy.active {
        return Err(Error::invalid(
            "operation_not_granted",
            "the Connection does not grant this operation",
        ));
    }
    // A final agent admission must load the exact issued selectors it named.
    // It may not mint a replacement snapshot id and then admit against it.
    let loaded = facts.loaded_snapshot.as_ref();
    if matches!(request, AdmissionRequestV1::Agent(_)) && loaded.is_none() {
        return Err(Error::invalid(
            "loaded_state_mismatch",
            "the operation did not name its issued snapshot and loaded receipt",
        ));
    }
    let mut runtime = facts.runtime.clone();
    let manifest_id = match loaded {
        Some(loaded) => {
            if facts.now_ms.get() >= loaded.snapshot.expires_at_ms.get() {
                return Err(Error::invalid(
                    "session_expired",
                    "the issued snapshot has expired",
                ));
            }
            if loaded.receipt.snapshot_id != loaded.snapshot.snapshot_id
                || loaded.receipt.manifest_digest != loaded.snapshot.manifest_digest
                || loaded.receipt.runtime_generation != loaded.snapshot.requester.runtime_generation
                || loaded.snapshot.requester.agent_uid != facts.requester.agent_uid
                || loaded.snapshot.requester.runtime_generation
                    != facts.requester.runtime_generation
                || loaded.receipt.observed_registry_digest != facts.runtime.observed_registry_digest
                || loaded.receipt.runtime_generation != facts.runtime.runtime_generation
                || loaded.receipt.enrollment_id != facts.runtime.enrollment_id
            {
                return Err(Error::invalid(
                    "loaded_state_mismatch",
                    "the loaded runtime does not match the issued receipt",
                ));
            }
            // A credential/grant/policy/manifest mutation after issuance
            // invalidates the snapshot: the authority revision must be the
            // one the snapshot was issued under.
            if loaded.authorization_revision != authority.authorization_revision
                || loaded.snapshot.policy_digest.as_str() != policy_row.policy_digest.as_str()
            {
                return Err(Error::invalid(
                    "stale_revision",
                    "the protected authorization changed after the snapshot was issued",
                ));
            }
            let Some(binding) = loaded
                .snapshot
                .connection_bindings
                .iter()
                .find(|binding| binding.connection_id.as_str() == connection_id)
            else {
                return Err(Error::invalid(
                    "loaded_state_mismatch",
                    "the Connection is not bound to the issued snapshot",
                ));
            };
            if binding.credential_revision.get() as i64 != connection.credential_revision
                || binding.authorization_revision != loaded.authorization_revision
            {
                return Err(Error::invalid(
                    "stale_revision",
                    "the credential revision changed after the snapshot was issued",
                ));
            }
            // Admission evidence windows come from the issued rows, never
            // from the caller or a freshly generated value.
            runtime.snapshot_issued_ms = loaded.snapshot.issued_at_ms;
            runtime.snapshot_expires_ms = loaded.snapshot.expires_at_ms;
            runtime.receipt_issued_ms = loaded.receipt.issued_at_ms;
            runtime.receipt_expires_ms = loaded.receipt.expires_at_ms;
            runtime.loaded_receipt_id = loaded.receipt.loaded_receipt_id.clone();
            runtime.runtime_generation = loaded.receipt.runtime_generation;
            runtime.enrollment_id = loaded.receipt.enrollment_id.clone();
            runtime.observed_registry_digest = loaded.receipt.observed_registry_digest.clone();
            if let Some(manifest_id) = facts.manifest_id.as_ref() {
                if *manifest_id != loaded.snapshot.manifest_id {
                    return Err(Error::invalid(
                        "loaded_state_mismatch",
                        "the manifest does not match the issued snapshot",
                    ));
                }
            }
            loaded.snapshot.manifest_id.clone()
        }
        None => match facts.manifest_id.as_ref() {
            Some(manifest_id) => manifest_id.clone(),
            None => approved_manifest_id_in(tx, &policy_row.policy_digest)?,
        },
    };
    let (manifest, manifest_state) =
        manifest_row_in(tx, manifest_id.as_str())?.ok_or_else(|| {
            Error::invalid("capability_unavailable", "the capability is not available")
        })?;
    if manifest_state != "approved"
        || manifest.policy_digest.as_str() != policy_row.policy_digest.as_str()
        || manifest.policy_digest != policy_row.policy.approved_document_digest
    {
        return Err(Error::invalid(
            "capability_unavailable",
            "the capability is not available",
        ));
    }
    if let Some(loaded) = loaded {
        if loaded.snapshot.manifest_digest != manifest.material_digest
            || loaded.snapshot.manifest_revision != manifest.revision
        {
            return Err(Error::invalid(
                "loaded_state_mismatch",
                "the approved manifest changed after the snapshot was issued",
            ));
        }
    }
    let grants = grants_in(
        tx,
        facts.requester.agent_uid.as_str(),
        &connection.platform,
        &connection.account,
    )?;
    let role_policy = role_model_policy_in(tx)?;
    let limits = config_limits_in(tx)?;
    let exposure = exposure_in(
        tx,
        facts.principal.principal_id.as_str(),
        &connection.connection_id,
        &authority.workspace_id,
        facts.now_ms.get(),
    )?;
    let existing = existing_dispatch_in(
        tx,
        &authority.workspace_id,
        facts.principal.custody_domain.as_str(),
        facts.principal.principal_id.as_str(),
        request.idempotency_key(),
    )?;

    // The approval is only read when the operation requires one; an absent
    // row leaves the planner's `ApprovalRequired` refusal intact.
    let approval = match facts.approval_id.as_ref() {
        Some(approval_id) => approval_row_in(tx, approval_id.as_str())?.map(|(row, _)| row),
        None => None,
    };
    let quote = match request {
        AdmissionRequestV1::Agent(invoke) => match &invoke.operation {
            AgentOperationV1::TextGenerate(operation) => {
                quote_row_in(tx, operation.quote_id.as_str())?
            }
            _ => None,
        },
        AdmissionRequestV1::HumanSmtpVerify(_) => None,
    };

    let connection_id_ident = ident(&connection.connection_id)?;
    let credential_revision =
        Revision::new(connection.credential_revision as u64).map_err(|_| int_refusal())?;
    let policy_revision =
        Revision::new(policy_row.policy_revision as u64).map_err(|_| int_refusal())?;
    let connection_facts = ConnectionFactsV1 {
        connection_id: connection_id_ident,
        provider_kind,
        workspace_id: ident(&authority.workspace_id)?,
        custody_domain: ident(facts.principal.custody_domain.as_str())?,
        account: ident(&connection.account)?,
        incarnation: ident(&format!(
            "{}@{}",
            connection.connection_id, connection.credential_revision
        ))?,
        credential_revision,
        authorization_revision: authority.authorization_revision,
        policy_revision,
        key_id: facts.key_id.clone(),
        exchange: if connection.exchange.is_empty() {
            None
        } else {
            Some(ident(&connection.exchange)?)
        },
        enabled: true,
        fence: FenceStateV1::Clear,
    };

    let verify_record = human_verify_record(
        tx,
        request,
        &connection_facts,
        policy_revision,
        &facts.resolved,
    )?;

    let snapshot = AdmissionSnapshotV1 {
        v: 1,
        snapshot_id: match loaded {
            Some(loaded) => loaded.snapshot.snapshot_id.clone(),
            None => random_uuid()?,
        },
        dispatch_id: facts.dispatch_id.clone(),
        request_id: facts.request_id.clone(),
        now_ms: facts.now_ms,
        clock_floor_ms: authority.clock_floor_ms,
        database_epoch: authority.database_epoch,
        ledger_head: authority.ledger_head.clone(),
        requester: facts.requester.clone(),
        principal: facts.principal.clone(),
        runtime: runtime.clone(),
        connection: connection_facts,
        policy: policy_row.policy.clone(),
        grants,
        role_policy,
        limits,
        exposure,
        manifest,
        observed_registry_digest: runtime.observed_registry_digest.clone(),
        policy_digest: Digest::try_from(policy_row.policy_digest.clone())
            .map_err(|_| int_refusal())?,
        resolved: facts.resolved.clone(),
        approval,
        test_send_record: None,
        verify_record,
        existing,
    };
    Ok((snapshot, quote))
}

/// The human verification record is the durable `credential_requests` row the
/// trusted entry path turned into this operation. It binds the exact
/// Connection, revisions and one-use request id; an absent or mismatched row
/// refuses. The body/destination digests are the actual resolved material, so
/// the planner's field-by-field comparison is meaningful.
fn human_verify_record(
    tx: &WriteTxn<'_>,
    request: &AdmissionRequestV1,
    connection: &ConnectionFactsV1,
    policy_revision: Revision,
    resolved: &ResolvedMaterialV1,
) -> Result<Option<HumanActionRecordV1>> {
    let AdmissionRequestV1::HumanSmtpVerify(verify) = request else {
        return Ok(None);
    };
    let row: Option<(String, Option<i64>, String)> = tx
        .query_opt(
            "SELECT connection_id,expected_credential_revision,status FROM credential_requests \
             WHERE verification_request_id=?1 AND verb='verify'",
            [verify.verification_request_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sql_error)?;
    let Some((connection_id, credential_revision, status)) = row else {
        return Ok(None);
    };
    if connection_id != verify.connection_id.as_str()
        || credential_revision != Some(verify.expected_credential_revision.get() as i64)
        || !matches!(status.as_str(), "submitting" | "unknown")
    {
        return Ok(None);
    }
    Ok(Some(HumanActionRecordV1 {
        record_id: ident(verify.verification_request_id.as_str())?,
        kind: OperationKindV1::SmtpVerify,
        connection_id: connection.connection_id.clone(),
        credential_revision: connection.credential_revision,
        authorization_revision: connection.authorization_revision,
        policy_revision,
        body_digest: resolved.provider_body_digest.clone(),
        destination_digest: cadence_core::connection_dispatch::destination_digest_for(
            &resolved.destination,
        ),
        human_decision_ref: ident(verify.verification_request_id.as_str())?,
    }))
}

/// Insert the immutable dispatch row.
fn insert_dispatch_in(
    tx: &WriteTxn<'_>,
    commit: &DispatchCommitV1,
    principal_key: &str,
    agent_uid: &str,
) -> Result<()> {
    let commit_json = serde_json::to_string(commit).map_err(|_| refused())?;
    tx.execute_raw(
        "INSERT INTO connection_dispatches(
           dispatch_id,request_id,workspace_id,custody_domain,principal_key,agent_uid,
           connection_id,credential_revision,authorization_revision,idempotency_key,
           idempotency_digest,payload_digest,body_digest,destination_digest,commitment_digest,
           commit_json,committed_ms,reserved_micros,state)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,'committed')",
        rusqlite::params![
            commit.dispatch_id.as_str(),
            commit.request_id.as_str(),
            commit.workspace_id.as_str(),
            commit.custody_domain.as_str(),
            principal_key,
            agent_uid,
            commit.connection_id.as_str(),
            commit.credential_revision.get() as i64,
            commit.authorization_revision.get() as i64,
            commit.idempotency_key.as_str(),
            commit.idempotency_digest.as_str(),
            commit.payload_digest.as_str(),
            commit.body_digest.as_str(),
            commit.destination_digest.as_str(),
            commit.commitment_digest.as_str(),
            commit_json,
            commit.request_timestamp_ms.get() as i64,
            commit.reserved_micros.get() as i64,
        ],
    )
    .map_err(sql_error)?;
    Ok(())
}

/// The authoritative commit: re-read every authorizing row inside one writer
/// transaction, plan, append the reservation to the real ledger head, bind the
/// commit to it, CAS the approval, and insert the immutable row.
pub(crate) fn commit_dispatch_in(
    tx: &WriteTxn<'_>,
    facts: &AdmissionFactsV1,
    request: AdmissionRequestV1,
) -> Result<CommitOutcome> {
    require_clean_journal_in(tx)?;
    // Seal any authorization changes an earlier writer queued in this same
    // transaction before a new head is bound.
    let _ = seal_authority_changes_in(tx)?;
    let (snapshot, quote) = snapshot_in(tx, facts, &request)?;
    let planned = plan_connection_dispatch_v1(&snapshot, request, quote.as_ref()).map_err(coded)?;
    let prepared = match planned {
        PlanOutcomeV1::Duplicate(status) => return Ok(CommitOutcome::Duplicate(status)),
        PlanOutcomeV1::Prepared(prepared) => prepared,
    };
    let bound = finish_prepared_in(tx, &prepared, facts)?;
    Ok(CommitOutcome::Committed(bound))
}

fn finish_prepared_in(
    tx: &WriteTxn<'_>,
    prepared: &PreparedDispatchV1,
    facts: &AdmissionFactsV1,
) -> Result<DispatchCommitV1> {
    // The reservation is appended with the immutable binding refs — never the
    // final head or commitment digest — so no hash cycle can exist.
    let head = append_record_in(
        tx,
        prepared.previous_head(),
        LedgerMutationKindV1::Reservation,
        prepared.binding_refs(),
    )?;
    let commit = prepared.finalize(&head).map_err(coded)?;
    if commit.dispatch_id != facts.dispatch_id {
        return Err(Error::invalid(
            "digest_mismatch",
            "the dispatch identity does not match the planned commitment",
        ));
    }
    insert_dispatch_in(
        tx,
        &commit,
        facts.principal.principal_id.as_str(),
        facts.requester.agent_uid.as_str(),
    )?;
    if let Some(approval_id) = commit.approval_id.as_ref() {
        // One approval, one dispatch. The trigger refuses a second consume even
        // if a future writer forgets the predicate.
        let consumed = tx
            .execute_raw(
                "UPDATE connection_send_approvals SET consumed_dispatch_id=?1 \
                 WHERE approval_id=?2 AND consumed_dispatch_id IS NULL",
                rusqlite::params![commit.dispatch_id.as_str(), approval_id.as_str()],
            )
            .map_err(sql_error)?;
        if consumed != 1 {
            return Err(Error::invalid(
                "approval_required",
                "a current human approval is required",
            ));
        }
        // A protected SMTP send executes one human-decided staged effect. Its
        // state change commits with the dispatch row and the approval CAS, so
        // no second transaction can claim the same decision.
        if commit.operation_kind == OperationKindV1::SmtpSend {
            let (approval, already, _recipient, _body) =
                approval_material_in(tx, approval_id.as_str())?.ok_or_else(|| {
                    Error::invalid("approval_required", "a current human approval is required")
                })?;
            if already.is_some() {
                return Err(Error::invalid(
                    "approval_required",
                    "a current human approval is required",
                ));
            }
            let effect = crate::store::effects::effect_row_in(tx, approval.source_id.as_str())?
                .ok_or_else(|| {
                    Error::invalid("approval_required", "a current human approval is required")
                })?;
            let binding = effect_operation_binding(
                commit.workspace_id.as_str(),
                &commit.principal_binding_digest,
                commit.connection_id.as_str(),
                commit.credential_revision,
                commit.authorization_revision,
                commit.policy_revision,
                approval.source_id.as_str(),
                &approval.source_digest,
                &approval.body_digest,
            );
            crate::store::effects::effect_executing_in(
                tx,
                approval.source_id.as_str(),
                &effect,
                Some(binding.as_str()),
            )?;
        }
    }
    // The durable clock floor is monotonic; the commit time is the request
    // time the planner accepted.
    tx.execute_raw(
        "UPDATE connection_metadata SET admission_clock_ms=?1 \
         WHERE singleton=1 AND admission_clock_ms<?1",
        rusqlite::params![facts.now_ms.get() as i64],
    )
    .map_err(sql_error)?;
    Ok(commit)
}

pub(crate) struct DispatchRow {
    pub commit: DispatchCommitV1,
    pub receipt: Option<DispatchReceiptV1>,
    pub state: DispatchStateV1,
}

pub(crate) fn dispatch_row_in(
    conn: &impl StoreConn,
    dispatch_id: &str,
    request_id: &str,
    principal_key: &str,
) -> Result<Option<DispatchRow>> {
    let row: Option<(String, String, Option<String>)> = conn
        .query_opt(
            "SELECT commit_json,state,receipt_json FROM connection_dispatches \
             WHERE dispatch_id=?1 AND request_id=?2 AND principal_key=?3",
            rusqlite::params![dispatch_id, request_id, principal_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sql_error)?;
    let Some((raw, state, receipt)) = row else {
        return Ok(None);
    };
    let commit = serde_json::from_str::<DispatchCommitV1>(&raw).map_err(|_| int_refusal())?;
    let receipt = match receipt {
        None => None,
        Some(raw) => {
            Some(serde_json::from_str::<DispatchReceiptV1>(&raw).map_err(|_| int_refusal())?)
        }
    };
    Ok(Some(DispatchRow {
        commit,
        receipt,
        state: parse_state(&state)?,
    }))
}

/// Settle the one claimed dispatch. A CAS from `committed` makes the
/// settlement win exactly once; an uncertain outcome stays reserved.
pub(crate) fn settle_dispatch_in(
    tx: &WriteTxn<'_>,
    commit: &DispatchCommitV1,
    outcome: ValidatedOutcomeV1,
) -> Result<SettlementV1> {
    let settlement = settle_connection_dispatch_v1(commit, outcome).map_err(coded)?;
    let receipt = &settlement.receipt;
    let state = state_str(receipt.state);
    let usage_json = match &receipt.usage {
        Some(usage) => Some(serde_json::to_string(usage).map_err(|_| refused())?),
        None => None,
    };
    let result_json = match &receipt.result {
        Some(result) => Some(serde_json::to_string(result).map_err(|_| refused())?),
        None => None,
    };
    let updated = tx
        .execute_raw(
            "UPDATE connection_dispatches SET state=?2,actual_micros=?3,usage_json=?4,\
             result_json=?5,error_code=?6,receipt_json=?7,settled_ms=?8 \
             WHERE dispatch_id=?1 AND state='committed'",
            rusqlite::params![
                commit.dispatch_id.as_str(),
                state,
                receipt.cost.actual_micros.map(|value| value.get() as i64),
                usage_json,
                result_json,
                receipt.error.as_ref().map(|error| error.code.as_str()),
                serde_json::to_string(receipt).map_err(|_| refused())?,
                now_ms()?,
            ],
        )
        .map_err(sql_error)?;
    if updated != 1 {
        return Err(Error::invalid(
            "state_conflict",
            "the dispatch state does not permit this transition",
        ));
    }
    let head = ledger_head_in(tx)?;
    let refs = vec![Digest::try_from(material_digest_for_domain(
        "cadence-protected-settlement-v1",
        &json!({
            "dispatch_id": commit.dispatch_id.as_str(),
            "commitment_digest": commit.commitment_digest.as_str(),
            "state": state,
            "actual_micros": receipt.cost.actual_micros.map(|value| value.get()),
        }),
    ))
    .map_err(|_| refused())?];
    let _ = append_record_in(tx, &head, LedgerMutationKindV1::Settlement, refs)?;
    if settlement.fence {
        // An overrun fences new protected admission; the history and the
        // actual validated cost stay readable for reconciliation.
        tx.execute_raw(
            "UPDATE connection_dispatch_config SET fenced=1 WHERE singleton=1",
            [],
        )
        .map_err(sql_error)?;
    }
    Ok(settlement)
}

// ---------------------------------------------------------------------------
// Checkpoint support
// ---------------------------------------------------------------------------

/// The owner's fixed control frame admits at most 64 prefix records and 32 KiB
/// of JSON. One checkpoint exchange is one BOUNDED suffix, never the whole
/// immutable history, so retained ledger length can never exhaust transport.
/// Local validation still covers the complete chain.
const CHECKPOINT_MAX_RECORDS: usize = 64;
/// Conservative share of the 32-KiB frame for the record array itself; the
/// checkpoint envelope and transport fields fit in the remainder.
const CHECKPOINT_MAX_RECORD_BYTES: usize = 24 * 1024;
/// One initial plan plus ONE read-only re-plan after a positively received
/// stale-watermark conflict. Uncertainty never re-plans; the same authenticated
/// watermark refusing twice is terminal.
const CHECKPOINT_MAX_ATTEMPTS: usize = 2;

fn ledger_mismatch() -> Error {
    Error::invalid("ledger_mismatch", "the protected ledger does not verify")
}

pub(crate) fn ledger_prefix_in(
    conn: &impl StoreConn,
    after: u64,
    through: u64,
) -> Result<Vec<LedgerRecordV1>> {
    if after > through {
        return Err(refused());
    }
    let raw: Vec<String> = conn
        .query_vec(
            "SELECT record_json FROM connection_ledger WHERE sequence>?1 AND sequence<=?2 \
             ORDER BY sequence",
            rusqlite::params![after as i64, through as i64],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let mut records = Vec::with_capacity(raw.len());
    for record in raw {
        records.push(serde_json::from_str::<LedgerRecordV1>(&record).map_err(|_| int_refusal())?);
    }
    Ok(records)
}

/// Resolve the authenticated owner watermark to a local anchor and prove it is
/// an exact point on the retained chain. The whole history is validated at
/// boot/preflight; a per-write checkpoint only re-reads this one record plus
/// the hash-linked suffix, so retained history length never makes a normal
/// write O(n). A watermark this store cannot locate is a divergence, never a
/// reason to lower or re-anchor the external floor.
fn owner_anchor_in(
    conn: &impl StoreConn,
    context: &ProtectedCheckpointContext,
    database_epoch: Revision,
) -> Result<Digest> {
    let minimum = context.minimum_sequence.get();
    if minimum == 0 {
        // The authenticated zero-sequence watermark is the epoch-bound
        // genesis anchor, not permission to accept an arbitrary chain.
        let genesis = genesis_ledger_head_v1(database_epoch);
        if genesis.digest != context.minimum_digest {
            return Err(ledger_mismatch());
        }
        return Ok(genesis.digest);
    }
    let record = ledger_record_in(conn, minimum)?;
    match record {
        Some(record)
            if record.sequence.get() == minimum && record.digest == context.minimum_digest =>
        {
            Ok(context.minimum_digest.clone())
        }
        _ => Err(ledger_mismatch()),
    }
}

fn ledger_record_in(conn: &impl StoreConn, sequence: u64) -> Result<Option<LedgerRecordV1>> {
    let raw: Option<String> = conn
        .query_opt(
            "SELECT record_json FROM connection_ledger WHERE sequence=?1",
            [sequence as i64],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    match raw {
        Some(raw) => Ok(Some(
            serde_json::from_str::<LedgerRecordV1>(&raw).map_err(|_| int_refusal())?,
        )),
        None => Ok(None),
    }
}

/// The records strictly after the authenticated watermark through exactly
/// `committed`, verified as one contiguous segment anchored at `anchor`.
fn verified_suffix_in(
    conn: &impl StoreConn,
    minimum: u64,
    committed: &LedgerHeadV1,
    anchor: &Digest,
) -> Result<Vec<LedgerRecordV1>> {
    let suffix = ledger_prefix_in(conn, minimum, committed.sequence.get())?;
    let first = suffix.first().ok_or_else(ledger_mismatch)?;
    if first.previous_digest != *anchor {
        return Err(ledger_mismatch());
    }
    let head = verify_ledger_chain_v1(&suffix).map_err(|_| ledger_mismatch())?;
    if head.sequence != committed.sequence
        || head.digest != committed.digest
        || head.database_epoch != committed.database_epoch
    {
        return Err(ledger_mismatch());
    }
    Ok(suffix)
}

/// The exact committed record must still exist locally when the owner
/// watermark is already past it (the receipt path).
fn require_local_record_in(conn: &impl StoreConn, head: &LedgerHeadV1) -> Result<()> {
    if head.sequence.get() == 0 {
        return Err(ledger_mismatch());
    }
    match ledger_record_in(conn, head.sequence.get())? {
        Some(record)
            if record.sequence == head.sequence
                && record.digest == head.digest
                && record.database_epoch == head.database_epoch =>
        {
            Ok(())
        }
        _ => Err(ledger_mismatch()),
    }
}

/// The checkpoint for one exact head. The equal-watermark exchange carries no
/// records at all, so the checkpoint is built from the head itself.
fn checkpoint_for_head(
    head: &LedgerHeadV1,
    workspace_id: &str,
    owner_epoch: Revision,
    runtime_binding_digest: &Digest,
) -> Result<CheckpointV1> {
    let checkpoint = CheckpointV1 {
        v: 1,
        workspace_id: ident(workspace_id)?,
        custody_domain: CustodyDomainV1::Local,
        database_epoch: head.database_epoch,
        owner_epoch: Some(owner_epoch),
        custody_epoch: None,
        ledger_head: head.clone(),
        runtime_binding_digest: runtime_binding_digest.clone(),
    };
    checkpoint.validate().map_err(|_| ledger_mismatch())?;
    Ok(checkpoint)
}

/// One bounded authenticated extension over a contiguous record slice. The
/// checkpoint names the slice's own last record so the owner advances its
/// watermark chunk by chunk; only the FINAL chunk names the actual committed
/// head and its acknowledgement is the one commit success binds.
fn checkpoint_chunk(
    records: &[LedgerRecordV1],
    workspace_id: &str,
    owner_epoch: Revision,
    runtime_binding_digest: &Digest,
) -> Result<CheckpointV1> {
    let last = records.last().ok_or_else(refused)?;
    checkpoint_for_head(
        &LedgerHeadV1 {
            sequence: last.sequence,
            database_epoch: last.database_epoch,
            digest: last.digest.clone(),
        },
        workspace_id,
        owner_epoch,
        runtime_binding_digest,
    )
}

/// An ACK for a dispatch commit must satisfy the unchanged shared
/// `checkpoint_matches_commit` binding; an ACK for a management head must bind
/// that exact head.
fn verify_ack_for(
    commit: Option<&DispatchCommitV1>,
    head: &LedgerHeadV1,
    ack: &CheckpointAckV1,
) -> Result<()> {
    match commit {
        Some(commit) => verify_checkpoint_ack_in(commit, ack),
        None => verify_checkpoint_head_in(head, ack),
    }
}

/// Partition the verified suffix into bounded chunks. Every chunk is a real
/// contiguous segment; the last one ends at the committed head. A non-empty
/// suffix always yields at least one chunk.
fn checkpoint_chunks(
    records: &[LedgerRecordV1],
    workspace_id: &str,
    owner_epoch: Revision,
    runtime_binding_digest: &Digest,
) -> Result<Vec<(CheckpointV1, Vec<LedgerRecordV1>)>> {
    let mut chunks = Vec::new();
    let mut current: Vec<LedgerRecordV1> = Vec::new();
    let mut bytes = 0usize;
    for record in records {
        let record_bytes = serde_json::to_vec(record).map_err(|_| refused())?.len();
        if record_bytes > CHECKPOINT_MAX_RECORD_BYTES {
            // A single record that cannot fit one authenticated frame could
            // never be acknowledged; refuse instead of truncating history.
            return Err(ledger_mismatch());
        }
        if !current.is_empty()
            && (current.len() >= CHECKPOINT_MAX_RECORDS
                || bytes + record_bytes > CHECKPOINT_MAX_RECORD_BYTES)
        {
            let checkpoint =
                checkpoint_chunk(&current, workspace_id, owner_epoch, runtime_binding_digest)?;
            chunks.push((checkpoint, std::mem::take(&mut current)));
            bytes = 0;
        }
        bytes += record_bytes;
        current.push(record.clone());
    }
    if records.is_empty() {
        return Err(ledger_mismatch());
    }
    let checkpoint = checkpoint_chunk(&current, workspace_id, owner_epoch, runtime_binding_digest)?;
    chunks.push((checkpoint, current));
    Ok(chunks)
}

/// Compare the authenticated external acknowledgement with the committed
/// dispatch row. Only a positive match may promote a prepared permit.
pub(crate) fn verify_checkpoint_ack_in(
    commit: &DispatchCommitV1,
    ack: &CheckpointAckV1,
) -> Result<()> {
    checkpoint_matches_commit(commit, ack).map_err(coded)
}

/// The acknowledgement must bind exactly the head this store checkpointed.
pub(crate) fn verify_checkpoint_head_in(head: &LedgerHeadV1, ack: &CheckpointAckV1) -> Result<()> {
    ack.validate()
        .map_err(|_| Error::invalid("ledger_mismatch", "the acknowledgement is malformed"))?;
    if ack.sequence != head.sequence
        || ack.digest != head.digest
        || ack.database_epoch != head.database_epoch
    {
        return Err(Error::invalid(
            "ledger_mismatch",
            "the external watermark acknowledgement does not match the protected head",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// External restore-watermark transport
// ---------------------------------------------------------------------------

/// The authenticated owner context one bounded checkpoint exchange needs. The
/// owner epoch is never the ledger's `database_epoch`; the minimum is the
/// owner's CURRENT external watermark, which the next suffix must follow
/// exactly.
#[derive(Clone, Debug)]
pub(crate) struct ProtectedCheckpointContext {
    pub owner_epoch: Revision,
    pub minimum_sequence: Counter,
    pub minimum_digest: Digest,
}

/// The daemon-installed external checkpoint transport. `prepare_store`
/// installs it before boot-time authorizing writes; the store uses it only
/// AFTER a writer transaction committed and released the database lock. It
/// carries no credential and cannot construct readiness.
pub(crate) trait ProtectedCheckpointWriter: Send + Sync {
    /// Live authenticated owner witness: workspace/database lineage, a stable
    /// restore state, the live owner epoch and the current external watermark.
    fn context(
        &self,
        workspace_id: &str,
        database_epoch: Revision,
    ) -> Result<ProtectedCheckpointContext>;

    /// Submit ONE bounded suffix. `records` is either empty (only when the
    /// owner watermark equals this checkpoint's head) or a contiguous segment
    /// of at most [`CHECKPOINT_MAX_RECORDS`] records whose first sequence is
    /// the owner's watermark + 1 and whose last record is exactly
    /// `checkpoint.ledger_head`. The reply must acknowledge that exact head.
    fn checkpoint(
        &self,
        checkpoint: &CheckpointV1,
        records: &[LedgerRecordV1],
    ) -> Result<CheckpointAckV1>;

    /// Read-only authenticated receipt for a checkpoint the owner ALREADY
    /// verified and persisted in a previously accepted suffix. Used only when
    /// a concurrent later checkpoint advanced the watermark past an older
    /// committed head. The checkpoint carries the PENDING commit's expected
    /// owner epoch, workspace, database epoch, local domain, exact target head
    /// and runtime binding; the owner requires its CURRENT epoch to match, so a
    /// reconnect after an owner restart cannot acknowledge a stale pending
    /// epoch. It never lowers the watermark and never inserts a checkpoint.
    fn acknowledged_head(&self, checkpoint: &CheckpointV1) -> Result<CheckpointAckV1>;
}

// ---------------------------------------------------------------------------
// Broker-issued snapshot and loaded receipt
// ---------------------------------------------------------------------------

/// Trusted-adapter facts for one issuance. Everything else — approved
/// manifest, active Connection policies, credential revisions, the durable
/// clock floor and the authorization revision — is read from the actual rows
/// inside the issuing transaction; ids and times are minted here.
pub(crate) struct SnapshotIssueFactsV1 {
    pub snapshot_id: UuidV1,
    pub loaded_receipt_id: UuidV1,
    pub now_ms: EpochMs,
    pub agent_uid: AgentUid,
    pub session_binding_id: UuidV1,
    pub runtime_generation: Revision,
    pub enrollment_id: UuidV1,
    pub observed_registry_digest: Digest,
    pub manifest_id: Ident,
}

#[derive(Clone, Debug)]
pub(crate) struct IssuedSnapshotV1 {
    pub snapshot: SnapshotV1,
    pub receipt: LoadedReceiptV1,
}

/// The exact issued rows one final admission re-loads. It is evidence, not a
/// permit: the dispatcher still crosses the full planner and commit.
#[derive(Clone, Debug)]
pub(crate) struct LoadedSnapshot {
    pub snapshot: SnapshotV1,
    pub receipt: LoadedReceiptV1,
    pub authorization_revision: Revision,
}

pub(crate) fn issue_snapshot_in(
    tx: &WriteTxn<'_>,
    facts: &SnapshotIssueFactsV1,
) -> Result<IssuedSnapshotV1> {
    require_clean_journal_in(tx)?;
    let _ = seal_authority_changes_in(tx)?;
    let authority = authority_in(tx)?;
    if !authority.enabled || !authority.ever_enabled || authority.fenced {
        return Err(Error::invalid(
            "runtime_unqualified",
            "protected Connections are not enabled on this store",
        ));
    }
    if facts.now_ms.get() < authority.clock_floor_ms.get() {
        return Err(Error::invalid(
            "clock_regression",
            "the durable admission clock floor is ahead of the request",
        ));
    }
    let (manifest, manifest_state) =
        manifest_row_in(tx, facts.manifest_id.as_str())?.ok_or_else(|| {
            Error::invalid("capability_unavailable", "the capability is not available")
        })?;
    if manifest_state != "approved"
        || manifest.manifest_id != facts.manifest_id
        || manifest.policy_digest.as_str().is_empty()
    {
        return Err(Error::invalid(
            "capability_unavailable",
            "the capability is not available",
        ));
    }
    cadence_core::connection_dispatch::dto::validate_manifest_v1(&manifest).map_err(coded)?;
    let policy_digest = manifest.policy_digest.clone();
    let rows: Vec<(String, String, i64, String)> = tx
        .query_vec(
            "SELECT p.connection_id,c.platform,c.credential_revision,p.policy_json \
             FROM connection_operation_policies p \
             JOIN platform_credentials c ON c.connection_id=p.connection_id \
             WHERE p.state='active' AND p.policy_digest=?1 ORDER BY p.connection_id",
            [policy_digest.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(sql_error)?;
    let mut connection_bindings = Vec::with_capacity(rows.len());
    let mut capability_connections = std::collections::BTreeSet::new();
    for (connection_id, platform, credential_revision, policy_json) in rows {
        if credential_revision <= 0 {
            return Err(int_refusal());
        }
        let provider_kind = provider_kind_of(&platform)?;
        let policy =
            serde_json::from_str::<OperationPolicyV1>(&policy_json).map_err(|_| int_refusal())?;
        if !policy.active || policy.allowed_operations.is_empty() {
            continue;
        }
        capability_connections.insert(connection_id.clone());
        connection_bindings.push(ConnectionBindingV1 {
            connection_id: ident(&connection_id)?,
            provider_kind,
            credential_revision: Revision::new(credential_revision as u64)
                .map_err(|_| int_refusal())?,
            authorization_revision: authority.authorization_revision,
            allowed_operations: policy.allowed_operations,
        });
    }
    let mut capabilities = Vec::with_capacity(manifest.capabilities.len());
    for capability in &manifest.capabilities {
        if capability_connections.contains(capability.connection_scope.as_str()) {
            capabilities.push(capability.clone());
        }
    }
    if connection_bindings.is_empty() || capabilities.is_empty() {
        return Err(Error::invalid(
            "capability_unavailable",
            "the capability is not available",
        ));
    }
    let issued = std::cmp::max(facts.now_ms.get(), authority.clock_floor_ms.get());
    let expires = issued
        .checked_add(SNAPSHOT_LIFETIME_MS)
        .ok_or_else(|| Error::invalid("clock_regression", "the clock is unusable"))?;
    let snapshot = SnapshotV1 {
        v: 1,
        snapshot_id: facts.snapshot_id.clone(),
        requester: RequesterV1 {
            agent_uid: facts.agent_uid.clone(),
            session_binding_id: facts.session_binding_id.clone(),
            runtime_generation: facts.runtime_generation,
        },
        manifest_id: manifest.manifest_id.clone(),
        manifest_revision: manifest.revision,
        manifest_digest: manifest.material_digest.clone(),
        policy_digest: policy_digest.clone(),
        connection_bindings,
        capabilities,
        issued_at_ms: EpochMs::new(issued).map_err(|_| int_refusal())?,
        expires_at_ms: EpochMs::new(expires).map_err(|_| int_refusal())?,
    };
    cadence_core::connection_dispatch::dto::validate_snapshot_v1(&snapshot).map_err(coded)?;
    let receipt = LoadedReceiptV1 {
        v: 1,
        loaded_receipt_id: facts.loaded_receipt_id.clone(),
        snapshot_id: snapshot.snapshot_id.clone(),
        manifest_digest: snapshot.manifest_digest.clone(),
        observed_registry_digest: facts.observed_registry_digest.clone(),
        runtime_generation: facts.runtime_generation,
        enrollment_id: facts.enrollment_id.clone(),
        issued_at_ms: snapshot.issued_at_ms,
        expires_at_ms: snapshot.expires_at_ms,
    };
    cadence_core::connection_dispatch::dto::validate_loaded_receipt_v1(&receipt, &snapshot)
        .map_err(coded)?;
    tx.execute_raw(
        "INSERT INTO connection_snapshots(
           snapshot_id,workspace_id,custody_domain,agent_uid,session_binding_id,
           runtime_generation,authorization_revision,manifest_id,manifest_revision,
           manifest_digest,policy_digest,observed_registry_digest,enrollment_id,
           snapshot_json,issued_ms,expires_ms)
         VALUES(?1,?2,'local',?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        rusqlite::params![
            snapshot.snapshot_id.as_str(),
            authority.workspace_id,
            snapshot.requester.agent_uid.as_str(),
            snapshot.requester.session_binding_id.as_str(),
            facts.runtime_generation.get() as i64,
            authority.authorization_revision.get() as i64,
            snapshot.manifest_id.as_str(),
            snapshot.manifest_revision.get() as i64,
            snapshot.manifest_digest.as_str(),
            snapshot.policy_digest.as_str(),
            receipt.observed_registry_digest.as_str(),
            receipt.enrollment_id.as_str(),
            serde_json::to_string(&snapshot).map_err(|_| refused())?,
            issued as i64,
            expires as i64,
        ],
    )
    .map_err(sql_error)?;
    tx.execute_raw(
        "INSERT INTO connection_loaded_receipts(
           loaded_receipt_id,snapshot_id,workspace_id,manifest_digest,
           observed_registry_digest,runtime_generation,enrollment_id,receipt_json,
           issued_ms,expires_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        rusqlite::params![
            receipt.loaded_receipt_id.as_str(),
            receipt.snapshot_id.as_str(),
            authority.workspace_id,
            receipt.manifest_digest.as_str(),
            receipt.observed_registry_digest.as_str(),
            facts.runtime_generation.get() as i64,
            receipt.enrollment_id.as_str(),
            serde_json::to_string(&receipt).map_err(|_| refused())?,
            issued as i64,
            expires as i64,
        ],
    )
    .map_err(sql_error)?;
    tx.execute_raw(
        "UPDATE connection_metadata SET admission_clock_ms=?1 \
         WHERE singleton=1 AND admission_clock_ms<?1",
        rusqlite::params![issued as i64],
    )
    .map_err(sql_error)?;
    Ok(IssuedSnapshotV1 { snapshot, receipt })
}

/// Re-load exactly the selector pair a caller named. It answers the issued
/// rows only; the final transaction still re-reads every authorizing row.
pub(crate) fn load_snapshot_in(
    conn: &impl StoreConn,
    snapshot_id: &str,
    receipt_id: &str,
) -> Result<Option<LoadedSnapshot>> {
    let row: Option<(String, i64)> = conn
        .query_opt(
            "SELECT snapshot_json,authorization_revision FROM connection_snapshots \
             WHERE snapshot_id=?1",
            [snapshot_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sql_error)?;
    let Some((snapshot_json, authorization_revision)) = row else {
        return Ok(None);
    };
    let receipt_json: Option<String> = conn
        .query_opt(
            "SELECT receipt_json FROM connection_loaded_receipts \
             WHERE loaded_receipt_id=?1 AND snapshot_id=?2",
            rusqlite::params![receipt_id, snapshot_id],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let Some(receipt_json) = receipt_json else {
        return Ok(None);
    };
    let snapshot = serde_json::from_str::<SnapshotV1>(&snapshot_json).map_err(|_| int_refusal())?;
    let receipt =
        serde_json::from_str::<LoadedReceiptV1>(&receipt_json).map_err(|_| int_refusal())?;
    if snapshot.snapshot_id.as_str() != snapshot_id
        || receipt.loaded_receipt_id.as_str() != receipt_id
        || receipt.snapshot_id != snapshot.snapshot_id
        || authorization_revision <= 0
    {
        return Err(int_refusal());
    }
    cadence_core::connection_dispatch::dto::validate_snapshot_v1(&snapshot).map_err(coded)?;
    cadence_core::connection_dispatch::dto::validate_loaded_receipt_v1(&receipt, &snapshot)
        .map_err(coded)?;
    Ok(Some(LoadedSnapshot {
        snapshot,
        receipt,
        authorization_revision: Revision::new(authorization_revision as u64)
            .map_err(|_| int_refusal())?,
    }))
}

/// The newest still-live issued pair in the caller's transaction.
pub(crate) fn current_snapshot_in(
    conn: &impl StoreConn,
    agent_uid: &str,
    runtime_generation: u64,
    now_ms: u64,
) -> Result<Option<LoadedSnapshot>> {
    let id: Option<(String, String)> = conn
        .query_opt(
            "SELECT s.snapshot_id,r.loaded_receipt_id \
             FROM connection_snapshots s \
             JOIN connection_loaded_receipts r ON r.snapshot_id=s.snapshot_id \
             WHERE s.agent_uid=?1 AND s.runtime_generation=?2 AND s.expires_ms>?3 \
             ORDER BY s.issued_ms DESC,s.rowid DESC,r.rowid DESC LIMIT 1",
            rusqlite::params![agent_uid, runtime_generation as i64, now_ms as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sql_error)?;
    match id {
        Some((snapshot_id, receipt_id)) => load_snapshot_in(conn, &snapshot_id, &receipt_id),
        None => Ok(None),
    }
}

/// The external restore watermark a protected boot must satisfy. Supplied by
/// the measured owner/control authority; never by a local file or flag.
#[derive(Clone, Debug)]
pub(crate) struct ExternalFloorV1 {
    pub workspace_id: String,
    pub min_sequence: u64,
    pub min_digest: Digest,
}

/// What a protected boot may do with this store. `MetadataOnly` keeps the
/// database readable while withholding protected execution; it is never a
/// fallback to legacy protected custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BootAuthority {
    Ready,
    MetadataOnly(&'static str),
}

/// Compare the ACTUAL local full ledger chain with the external minimum.
/// Reads only: it never repairs, resets, imports or advances anything.
fn boot_preflight_in(
    conn: &impl StoreConn,
    floor: &ExternalFloorV1,
    require_enabled: bool,
) -> Result<BootAuthority> {
    let authority = match authority_in(conn) {
        Ok(authority) => authority,
        Err(_) => return Ok(BootAuthority::MetadataOnly("ledger_mismatch")),
    };
    if authority.fenced {
        return Ok(BootAuthority::MetadataOnly("protected_fenced"));
    }
    if authority.workspace_id != floor.workspace_id {
        return Ok(BootAuthority::MetadataOnly("restore_floor_mismatch"));
    }
    let head = authority.ledger_head;
    if head.sequence.get() < floor.min_sequence {
        return Ok(BootAuthority::MetadataOnly("restore_floor_missing"));
    }
    let prefix = ledger_prefix_in(conn, 0, head.sequence.get())?;
    let verified = if let Some(first) = prefix.first() {
        let genesis = genesis_ledger_head_v1(first.database_epoch);
        if first.sequence.get() != 1 || first.previous_digest != genesis.digest {
            return Ok(BootAuthority::MetadataOnly("ledger_mismatch"));
        }
        match verify_ledger_chain_v1(&prefix) {
            Ok(verified) => verified,
            Err(_) => return Ok(BootAuthority::MetadataOnly("ledger_mismatch")),
        }
    } else {
        genesis_ledger_head_v1(authority.database_epoch)
    };
    if verified.sequence != head.sequence
        || verified.digest != head.digest
        || verified.database_epoch != authority.database_epoch
    {
        return Ok(BootAuthority::MetadataOnly("ledger_mismatch"));
    }
    let included = if floor.min_sequence == 0 {
        // The authenticated zero-sequence watermark is the epoch-bound
        // genesis anchor, not permission to accept an arbitrary chain.
        let anchor = prefix
            .first()
            .map(|record| &record.previous_digest)
            .unwrap_or(&head.digest);
        anchor == &floor.min_digest
    } else {
        prefix
            .iter()
            .find(|record| record.sequence.get() == floor.min_sequence)
            .is_some_and(|record| record.digest == floor.min_digest)
    };
    if !included {
        return Ok(BootAuthority::MetadataOnly("restore_floor_mismatch"));
    }
    if require_enabled && (!authority.enabled || !authority.ever_enabled) {
        return Ok(BootAuthority::MetadataOnly("protected_disabled"));
    }
    Ok(BootAuthority::Ready)
}

// ---------------------------------------------------------------------------
// Management writers (the daemon proves operator/human authority first)
// ---------------------------------------------------------------------------

pub(crate) fn set_config_in(
    tx: &WriteTxn<'_>,
    enabled: bool,
    limits: &OwnerLimitsV1,
) -> Result<()> {
    let raw = serde_json::to_string(limits).map_err(|_| refused())?;
    tx.execute_raw(
        "UPDATE connection_dispatch_config SET enabled=?1,ever_enabled=(ever_enabled OR ?1),\
         limits_json=?2 WHERE singleton=1",
        rusqlite::params![i64::from(enabled), raw],
    )
    .map_err(sql_error)?;
    Ok(())
}

pub(crate) fn set_manifest_in(tx: &WriteTxn<'_>, manifest: &ManifestV1) -> Result<()> {
    let raw = serde_json::to_string(manifest).map_err(|_| refused())?;
    let existing: Option<(i64, String)> = tx
        .query_opt(
            "SELECT revision,material_digest FROM connection_manifests WHERE manifest_id=?1",
            [manifest.manifest_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sql_error)?;
    if let Some((revision, digest)) = existing {
        if revision == manifest.revision.get() as i64 && digest == manifest.material_digest.as_str()
        {
            return Ok(());
        }
        return Err(Error::invalid(
            "state_conflict",
            "approved manifest material is immutable",
        ));
    }
    tx.execute_raw(
        "INSERT INTO connection_manifests(
           manifest_id,revision,policy_digest,material_digest,manifest_json,state)
         VALUES(?1,?2,?3,?4,?5,'approved')",
        rusqlite::params![
            manifest.manifest_id.as_str(),
            manifest.revision.get() as i64,
            manifest.policy_digest.as_str(),
            manifest.material_digest.as_str(),
            raw,
        ],
    )
    .map_err(sql_error)?;
    Ok(())
}

pub(crate) fn set_policy_in(
    tx: &WriteTxn<'_>,
    connection_id: &str,
    credential_revision: u64,
    policy: &OperationPolicyV1,
    policy_digest: &str,
) -> Result<()> {
    let raw = serde_json::to_string(policy).map_err(|_| refused())?;
    tx.execute_raw(
        "INSERT INTO connection_operation_policies(
           connection_id,credential_revision,policy_revision,policy_digest,policy_json,state)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(connection_id) DO UPDATE SET
           credential_revision=excluded.credential_revision,
           policy_revision=excluded.policy_revision,
           policy_digest=excluded.policy_digest,
           policy_json=excluded.policy_json,
           state=excluded.state",
        rusqlite::params![
            connection_id,
            credential_revision as i64,
            policy.policy_revision.get() as i64,
            policy_digest,
            raw,
            if policy.active { "active" } else { "revoked" },
        ],
    )
    .map_err(sql_error)?;
    Ok(())
}

pub(crate) fn put_quote_in(
    tx: &WriteTxn<'_>,
    quote: &ModelQuoteV1,
    principal_key: &str,
    process_activation: &str,
) -> Result<()> {
    let raw = serde_json::to_string(quote).map_err(|_| refused())?;
    let digest = material_digest_value(
        "cadence-connection-model-quote-v1",
        &serde_json::to_value(quote).map_err(|_| refused())?,
    );
    tx.execute_raw(
        "INSERT OR REPLACE INTO connection_model_quotes(
           quote_id,connection_id,credential_revision,authorization_revision,principal_key,
           process_activation,quote_digest,quote_json,observed_ms,expires_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        rusqlite::params![
            quote.quote_id.as_str(),
            quote.connection_id.as_str(),
            quote.credential_revision.get() as i64,
            quote.authorization_revision.get() as i64,
            principal_key,
            process_activation,
            digest.as_str(),
            raw,
            quote.metadata_observed_ms.get() as i64,
            quote.metadata_expires_ms.get() as i64,
        ],
    )
    .map_err(sql_error)?;
    Ok(())
}

/// The single canonical private binding between one human-decided effect and
/// one protected SMTP dispatch. Stage stamps it; the final transaction
/// recomputes and requires it. Not an approval and not a permit.
#[allow(clippy::too_many_arguments)] // the binding mirrors the frozen operation schema
pub(crate) fn effect_operation_binding(
    workspace_id: &str,
    principal_digest: &Digest,
    connection_id: &str,
    credential_revision: Revision,
    authorization_revision: Revision,
    policy_revision: Revision,
    source_id: &str,
    source_digest: &Digest,
    body_digest: &Digest,
) -> Digest {
    Digest::try_from(material_digest_for_domain(
        "cadence-protected-effect-binding-v1",
        &json!({
            "v": 1,
            "workspace_id": workspace_id,
            "principal_digest": principal_digest.as_str(),
            "connection_id": connection_id,
            "credential_revision": credential_revision.get(),
            "authorization_revision": authorization_revision.get(),
            "policy_revision": policy_revision.get(),
            "source_id": source_id,
            "source_digest": source_digest.as_str(),
            "body_digest": body_digest.as_str(),
        }),
    ))
    .unwrap_or_else(|_| Digest::try_from(format!("sha256:{}", "0".repeat(64))).expect("canonical"))
}

pub(crate) fn put_approval_in(
    tx: &WriteTxn<'_>,
    approval: &SendApprovalV1,
    principal_key: &str,
    recipient: &str,
    body: &[u8],
) -> Result<()> {
    let raw = serde_json::to_string(approval).map_err(|_| refused())?;
    tx.execute_raw(
        "INSERT INTO connection_send_approvals(
           approval_id,principal_key,connection_id,credential_revision,authorization_revision,
           source_id,source_digest,recipient,body,approval_json,consumed_dispatch_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,NULL)
         ON CONFLICT(approval_id) DO NOTHING",
        rusqlite::params![
            approval.approval_id.as_str(),
            principal_key,
            approval.connection_id.as_str(),
            approval.credential_revision.get() as i64,
            approval.authorization_revision.get() as i64,
            approval.source_id.as_str(),
            approval.source_digest.as_str(),
            recipient,
            body,
            raw,
        ],
    )
    .map_err(sql_error)?;
    Ok(())
}

/// The frozen recipient and EXACT committed message bytes one approval binds.
/// Read only inside the trusted adapter; never a public projection.
/// The frozen approval payload plus its consumed dispatch, recipient and body.
pub(crate) type ApprovalMaterialV1 = (SendApprovalV1, Option<String>, String, Vec<u8>);

pub(crate) fn approval_material_in(
    conn: &impl StoreConn,
    approval_id: &str,
) -> Result<Option<ApprovalMaterialV1>> {
    let row: Option<(String, Option<String>, String, Vec<u8>)> = conn
        .query_opt(
            "SELECT approval_json,consumed_dispatch_id,recipient,body \
             FROM connection_send_approvals WHERE approval_id=?1",
            [approval_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(sql_error)?;
    let Some((raw, consumed, recipient, body)) = row else {
        return Ok(None);
    };
    let approval = serde_json::from_str::<SendApprovalV1>(&raw).map_err(|_| int_refusal())?;
    Ok(Some((approval, consumed, recipient, body)))
}

// ---------------------------------------------------------------------------
// Store-level entry points
// ---------------------------------------------------------------------------

impl Store {
    /// The authoritative protected state, read from actual rows. The monotone
    /// PROCESS admission fence is always reflected in `fenced`, even when the
    /// durable fence row could not be written.
    pub(crate) fn protected_authority(&self) -> Result<Authority> {
        let mut authority = self.read_tx(|conn| authority_in(&*conn))?;
        if self.admission_fenced() {
            authority.fenced = true;
        }
        Ok(authority)
    }

    /// The authoritative commit transaction. This is the ONLY place a
    /// dispatch becomes durable, and it returns data — never a permit. The
    /// process admission fence is re-checked INSIDE the writer transaction so
    /// an unacknowledged earlier authorizing outcome can never be followed by
    /// a new admission.
    pub(crate) fn protected_commit(
        &self,
        facts: AdmissionFactsV1,
        request: AdmissionRequestV1,
    ) -> Result<CommitOutcome> {
        self.require_admission_open()?;
        self.write_tx_pending_checkpoint(move |tx| {
            self.require_admission_open()?;
            commit_dispatch_in(tx, &facts, request)
        })
    }

    /// Settle one dispatch in its own short CAS transaction.
    pub(crate) fn protected_settle(
        &self,
        commit: DispatchCommitV1,
        outcome: ValidatedOutcomeV1,
    ) -> Result<SettlementV1> {
        self.write_tx(move |tx| settle_dispatch_in(tx, &commit, outcome))
    }

    /// The commit and receipt of an already-committed dispatch belonging to
    /// this requester, for reconciliation and re-authorization by the caller.
    pub(crate) fn protected_reconcile(
        &self,
        dispatch_id: &str,
        request_id: &str,
        principal_key: &str,
    ) -> Result<Option<DispatchRow>> {
        self.read_tx(|conn| dispatch_row_in(&*conn, dispatch_id, request_id, principal_key))
    }

    /// Acknowledge the EXACT committed dispatch head through the authenticated
    /// owner transport and prove the external floor covers it. Bounded
    /// suffixes keep the exchange inside the owner's frame no matter how long
    /// the retained ledger is; a concurrent later checkpoint is answered with
    /// a real owner receipt for this older head, never a fabricated ACK.
    pub(crate) fn protected_acknowledge_commit(
        &self,
        commit: &DispatchCommitV1,
        owner_epoch: Revision,
        runtime_binding_digest: Digest,
    ) -> Result<()> {
        self.require_admission_open()?;
        let committed = LedgerHeadV1 {
            sequence: commit.ledger_sequence,
            database_epoch: commit.database_epoch,
            digest: commit.ledger_digest.clone(),
        };
        self.checkpoint_committed_head(
            commit.workspace_id.as_str(),
            Some(owner_epoch),
            &runtime_binding_digest,
            &committed,
            Some(commit),
        )
        .map_err(|error| self.unacknowledged_authority(error))
    }

    /// Install the external watermark transport exactly once. Called by the
    /// daemon's pre-boot `prepare_store`, before any authorizing write. It also
    /// durably selects protected authority journaling (`protection_required`),
    /// independently of dispatch enablement, so first-secret/source enrollment
    /// and rotation before the first enable are still journaled.
    ///
    /// The marker write is sealed but deliberately not acknowledged on its own:
    /// the boot authority (owner floor) is decided immediately afterwards, and
    /// the normal guarded-recovery/authorizing checkpoint carries this record.
    /// A store that cannot record the selection refuses installation rather
    /// than running half-gated.
    pub(crate) fn install_protected_checkpoint(
        &self,
        writer: Arc<dyn ProtectedCheckpointWriter>,
    ) -> Result<()> {
        self.protected_checkpoint.set(writer).map_err(|_| {
            Error::invalid(
                "state_conflict",
                "the protected checkpoint transport is already installed",
            )
        })?;
        self.write_tx_pending_checkpoint(|tx| {
            tx.execute_raw(
                "UPDATE connection_dispatch_config SET protection_required=1 \
                 WHERE singleton=1 AND protection_required=0",
                [],
            )
            .map_err(sql_error)?;
            Ok(())
        })
    }

    pub(crate) fn protected_checkpoint_installed(&self) -> bool {
        self.protected_checkpoint.get().is_some()
    }

    /// Issue one immutable snapshot plus its contained loaded receipt from the
    /// actual approved rows. Server UUIDs and times only. This is execution
    /// material, so a process admission fence refuses it.
    pub(crate) fn protected_issue_snapshot(
        &self,
        facts: SnapshotIssueFactsV1,
    ) -> Result<IssuedSnapshotV1> {
        self.require_admission_open()?;
        self.write_tx(move |tx| {
            self.require_admission_open()?;
            issue_snapshot_in(tx, &facts)
        })
    }

    /// Load exactly the issued selector pair a caller named.
    pub(crate) fn protected_load_snapshot(
        &self,
        snapshot_id: &str,
        receipt_id: &str,
    ) -> Result<Option<LoadedSnapshot>> {
        self.read_tx(|conn| load_snapshot_in(&*conn, snapshot_id, receipt_id))
    }

    /// The newest still-live issued pair for one exact runtime subject. Used
    /// only to reuse current immutable material; it never mints authority.
    pub(crate) fn protected_current_snapshot(
        &self,
        agent_uid: &str,
        runtime_generation: u64,
        now_ms: u64,
    ) -> Result<Option<LoadedSnapshot>> {
        self.read_tx(|conn| current_snapshot_in(&*conn, agent_uid, runtime_generation, now_ms))
    }

    /// The newest still-live issued pair for one stable agent, without a
    /// runtime generation. Used only to revalidate held material.
    pub(crate) fn protected_current_snapshot_for_agent(
        &self,
        agent_uid: &str,
        now_ms: u64,
    ) -> Result<Option<LoadedSnapshot>> {
        self.read_tx(|conn| {
            let id: Option<(String, String)> = conn
                .query_opt(
                    "SELECT s.snapshot_id,r.loaded_receipt_id \
                     FROM connection_snapshots s \
                     JOIN connection_loaded_receipts r ON r.snapshot_id=s.snapshot_id \
                     WHERE s.agent_uid=?1 AND s.expires_ms>?2 \
                     ORDER BY s.issued_ms DESC,s.rowid DESC,r.rowid DESC LIMIT 1",
                    rusqlite::params![agent_uid, now_ms as i64],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(sql_error)?;
            match id {
                Some((snapshot_id, receipt_id)) => {
                    load_snapshot_in(&*conn, &snapshot_id, &receipt_id)
                }
                None => Ok(None),
            }
        })
    }

    /// The single approved manifest bound to an active reviewed policy. An
    /// ambiguous or absent answer is a refusal, never a guess.
    pub(crate) fn protected_single_approved_manifest(&self) -> Result<Option<Ident>> {
        self.read_tx(|conn| {
            let digest: Option<String> = conn
                .query_opt(
                    "SELECT policy_digest FROM connection_operation_policies \
                     WHERE state='active' ORDER BY connection_id LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            match digest {
                Some(digest) => Ok(Some(approved_manifest_id_in(&*conn, &digest)?)),
                None => Ok(None),
            }
        })
    }

    /// The frozen recipient and exact committed bytes of one approval.
    pub(crate) fn protected_approval_material(
        &self,
        approval_id: &str,
    ) -> Result<Option<ApprovalMaterialV1>> {
        self.read_tx(|conn| approval_material_in(&*conn, approval_id))
    }

    /// Requests in the sliding quote-read window for one principal and the
    /// whole workspace. Counted from the durable quote rows; no cached
    /// counter can drift.
    pub(crate) fn protected_quote_counts(
        &self,
        principal_key: &str,
        now_ms: u64,
    ) -> Result<(u64, u64)> {
        self.read_tx(|conn| {
            let lower = now_ms.saturating_sub(60_000) as i64;
            let now = now_ms as i64;
            let principal: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM connection_model_quotes \
                     WHERE principal_key=?1 AND observed_ms>?2 AND observed_ms<=?3",
                    rusqlite::params![principal_key, lower, now],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            let workspace: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM connection_model_quotes \
                     WHERE observed_ms>?1 AND observed_ms<=?2",
                    rusqlite::params![lower, now],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            if principal < 0 || workspace < 0 {
                return Err(int_refusal());
            }
            Ok((principal as u64, workspace as u64))
        })
    }

    /// The protected boot decision for an already-open, unrecovered store.
    /// Reads only; never repairs or advances the chain.
    pub(crate) fn protected_boot_preflight(
        &self,
        floor: &ExternalFloorV1,
    ) -> Result<BootAuthority> {
        if self.admission_fenced() {
            // A process admission fence outlives any durable row that failed to
            // persist; execution must never be authorized afterwards.
            return Ok(BootAuthority::MetadataOnly("protected_fenced"));
        }
        if !self.protected_checkpoint_installed() {
            return Ok(BootAuthority::MetadataOnly("checkpoint_uninstalled"));
        }
        self.read_tx(|conn| boot_preflight_in(&*conn, floor, true))
    }

    /// Human management and unrecovered startup still require the complete
    /// authentic history, but do not depend on the enablement they create.
    /// This never grants runtime or provider execution authority, and it stays
    /// available after a process admission fence so an operator can reconcile;
    /// [`Self::protected_authority`] reports that fence truthfully.
    pub(crate) fn protected_history_preflight(
        &self,
        floor: &ExternalFloorV1,
    ) -> Result<BootAuthority> {
        if !self.protected_checkpoint_installed() {
            return Ok(BootAuthority::MetadataOnly("checkpoint_uninstalled"));
        }
        self.read_tx(|conn| boot_preflight_in(&*conn, floor, false))
    }

    /// Append one explicit management journal record — used by key
    /// activation/unlock/lock, which change no authorizing row — and require
    /// its external acknowledgement.
    pub(crate) fn protected_record_management(
        &self,
        workspace_id: &str,
        owner_epoch: Revision,
        action_digest: Digest,
    ) -> Result<()> {
        let binding = action_digest.clone();
        self.protected_management_commit(workspace_id, owner_epoch, binding, move |tx| {
            let head = ledger_head_in(tx)?;
            let _ = append_record_in(
                tx,
                &head,
                LedgerMutationKindV1::Authorization,
                vec![action_digest],
            )?;
            Ok(())
        })
    }

    /// Checkpoint the CURRENT head and require the authenticated
    /// acknowledgement. Called after an authorizing management mutation has
    /// committed and released the lock.
    pub(crate) fn protected_checkpoint_head(
        &self,
        workspace_id: &str,
        owner_epoch: Revision,
        runtime_binding_digest: Digest,
    ) -> Result<()> {
        self.require_admission_open()?;
        let head = self.read_tx(|conn| ledger_head_in(&*conn))?;
        self.checkpoint_committed_head(
            workspace_id,
            Some(owner_epoch),
            &runtime_binding_digest,
            &head,
            None,
        )
        .map_err(|error| self.unacknowledged_authority(error))
    }

    /// Commit one authorizing management mutation, then checkpoint its actual
    /// immutable head before acknowledging success. A missing/lost
    /// acknowledgement fences further protected admission and answers
    /// `outcome_unknown`; the callback is never re-run.
    pub(crate) fn protected_management_commit<F, R: 'static>(
        &self,
        workspace_id: &str,
        owner_epoch: Revision,
        runtime_binding_digest: Digest,
        mutation: F,
    ) -> Result<R>
    where
        F: FnOnce(&WriteTxn<'_>) -> Result<R>,
    {
        let (head, value) = self.write_tx_pending_checkpoint(move |tx| {
            let value = mutation(tx)?;
            // Seal inside this transaction so the returned head is the one
            // that actually committed (the generic wrapper's second seal is a
            // no-op).
            let _ = seal_authority_changes_in(tx)?;
            Ok((ledger_head_in(tx)?, value))
        })?;
        self.checkpoint_authority_head(workspace_id, owner_epoch, runtime_binding_digest, head)?;
        Ok(value)
    }

    /// Ordinary app/grant/settings/background writers use the same external
    /// restore floor as explicit management and dispatch, before returning.
    pub(super) fn checkpoint_authority_write(&self, head: LedgerHeadV1) -> Result<()> {
        let checkpointed = (|| -> Result<()> {
            let workspace = self.connection_workspace_id()?;
            let binding = Digest::try_from(material_digest_for_domain(
                "cadence-protected-authority-checkpoint-v1",
                &serde_json::json!({"v": 1, "workspace_id": workspace, "head": head}),
            ))
            .map_err(|_| refused())?;
            self.checkpoint_committed_head(&workspace, None, &binding, &head, None)
        })();
        checkpointed.map_err(|error| self.unacknowledged_authority(error))
    }

    fn checkpoint_authority_head(
        &self,
        workspace_id: &str,
        owner_epoch: Revision,
        runtime_binding_digest: Digest,
        head: LedgerHeadV1,
    ) -> Result<()> {
        self.checkpoint_committed_head(
            workspace_id,
            Some(owner_epoch),
            &runtime_binding_digest,
            &head,
            None,
        )
        .map_err(|error| self.unacknowledged_authority(error))
    }

    /// Advance the authenticated external watermark to `committed`, or prove
    /// the owner already acknowledged that exact head. The full retained
    /// history is validated by boot/preflight; here the authenticated anchor
    /// record plus the hash-linked suffix and the exact committed record are
    /// checked, so the per-write cost does not grow with retained history.
    /// Nothing is retried after an uncertain reply, and any failure after the
    /// mutation committed is `outcome_unknown` plus a monotone admission fence.
    ///
    /// A POSITIVELY received owner conflict (stale watermark) may be re-planned
    /// from a fresh read-only context, without re-running the authorizing
    /// callback: the committed history is genuine and the next plan is
    /// suffix/equal/receipt again. A transport failure, malformed ACK or any
    /// other uncertainty never enters that path and is never satisfied by a
    /// later receipt.
    ///
    /// - watermark below the head: authenticated contiguous suffixes, each a
    ///   real checkpoint the owner verifies; only the final one names the
    ///   committed head and its ACK is accepted;
    /// - watermark equal: the equal-head checkpoint with an empty suffix is
    ///   the real owner ACK;
    /// - watermark above (a concurrent later checkpoint won): only the
    ///   owner's own authenticated receipt for this EXACT pending checkpoint
    ///   acknowledges it. Inclusion alone is not an ACK and is refused.
    fn checkpoint_committed_head(
        &self,
        workspace_id: &str,
        expected_owner_epoch: Option<Revision>,
        runtime_binding_digest: &Digest,
        committed: &LedgerHeadV1,
        commit: Option<&DispatchCommitV1>,
    ) -> Result<()> {
        if self.connection_workspace_id()? != workspace_id {
            return Err(ledger_mismatch());
        }
        let writer = self.protected_checkpoint.get().ok_or_else(|| {
            Error::invalid(
                "runtime_unqualified",
                "no protected checkpoint transport is installed",
            )
        })?;
        let mut refused_at: Option<(u64, Digest)> = None;
        for _ in 0..CHECKPOINT_MAX_ATTEMPTS {
            let context = writer.context(workspace_id, committed.database_epoch)?;
            if let Some(expected) = expected_owner_epoch {
                if context.owner_epoch != expected {
                    return Err(Error::invalid(
                        "runtime_unqualified",
                        "the authenticated owner epoch changed under the protected commit",
                    ));
                }
            }
            match self.exchange_committed_head(
                workspace_id,
                &context,
                runtime_binding_digest,
                committed,
                commit,
                writer.as_ref(),
            ) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == "conflict" => {
                    // Definite refusal on the owner's watermark: re-plan from
                    // a fresh read-only context. The same authenticated
                    // watermark refusing again cannot be helped by another
                    // read.
                    let key = (
                        context.minimum_sequence.get(),
                        context.minimum_digest.clone(),
                    );
                    if refused_at.as_ref() == Some(&key) {
                        return Err(error);
                    }
                    refused_at = Some(key);
                }
                // Transport loss, malformed reply, busy/refused or any other
                // uncertainty: never retried.
                Err(error) => return Err(error),
            }
        }
        Err(Error::invalid(
            "runtime_unqualified",
            "the owner watermark kept moving; the protected outcome is unacknowledged",
        ))
    }

    /// One plan against one authenticated context: bounded suffix, exact equal
    /// head, or read-only historical receipt.
    fn exchange_committed_head(
        &self,
        workspace_id: &str,
        context: &ProtectedCheckpointContext,
        runtime_binding_digest: &Digest,
        committed: &LedgerHeadV1,
        commit: Option<&DispatchCommitV1>,
        writer: &dyn ProtectedCheckpointWriter,
    ) -> Result<()> {
        let minimum = context.minimum_sequence.get();
        if minimum > committed.sequence.get() {
            // A concurrent later checkpoint advanced the owner past this
            // committed head. Only a real owner receipt for this EXACT pending
            // checkpoint can acknowledge it. A receipt refusal is definitive
            // (no proven row), not a watermark race, so it is reported as
            // non-conflict and never re-planned.
            self.read_tx(|conn| require_local_record_in(&*conn, committed))?;
            let checkpoint = checkpoint_for_head(
                committed,
                workspace_id,
                context.owner_epoch,
                runtime_binding_digest,
            )?;
            let ack = match writer.acknowledged_head(&checkpoint) {
                Ok(ack) => ack,
                Err(error) if error.kind() == "conflict" => {
                    return Err(Error::invalid(
                        "runtime_unqualified",
                        "the owner holds no proven receipt for this committed head",
                    ))
                }
                Err(error) => return Err(error),
            };
            return verify_ack_for(commit, committed, &ack);
        }
        let anchor =
            self.read_tx(|conn| owner_anchor_in(&*conn, context, committed.database_epoch))?;
        if minimum == committed.sequence.get() {
            // Nothing to extend. The owner's equal-head acceptance is a real
            // ACK; a zero head has no mutation to acknowledge.
            if committed.sequence.get() == 0 {
                return Ok(());
            }
            if anchor != committed.digest {
                return Err(ledger_mismatch());
            }
            let checkpoint = checkpoint_for_head(
                committed,
                workspace_id,
                context.owner_epoch,
                runtime_binding_digest,
            )?;
            let ack = writer.checkpoint(&checkpoint, &[])?;
            return verify_ack_for(commit, committed, &ack);
        }
        let records =
            self.read_tx(|conn| verified_suffix_in(&*conn, minimum, committed, &anchor))?;
        let chunks = checkpoint_chunks(
            &records,
            workspace_id,
            context.owner_epoch,
            runtime_binding_digest,
        )?;
        let total = chunks.len();
        for (index, (checkpoint, records)) in chunks.into_iter().enumerate() {
            let ack = writer.checkpoint(&checkpoint, &records)?;
            verify_checkpoint_head_in(&checkpoint.ledger_head, &ack)?;
            if index + 1 == total {
                // The final exchange names the actual committed head; its ACK
                // is the only one commit success binds.
                verify_ack_for(commit, committed, &ack)?;
            }
        }
        Ok(())
    }

    /// The monotone PROCESS admission fence (NWAR-6). Set the instant a
    /// protected authorizing outcome becomes unknown; it is never cleared in
    /// this process, so later eligibility/admission cannot rely on a durable
    /// fence row that failed to persist.
    fn fence_admission(&self) {
        self.admission_fence
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn admission_fenced(&self) -> bool {
        self.admission_fence
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Execution-admission gate. Metadata/status bookkeeping deliberately
    /// stays available after a fence so the operator can reconcile.
    fn require_admission_open(&self) -> Result<()> {
        if self.admission_fenced() {
            Err(Error::invalid(
                "runtime_revoked",
                "protected admission is fenced: the previous authorizing outcome is unacknowledged",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn unacknowledged_authority(&self, error: Error) -> Error {
        // Retain the monotone process admission fence BEFORE the durable
        // attempt: the guarded local fence below may itself fail (BUSY or I/O),
        // and that must never re-permit admission of unacknowledged authority.
        self.fence_admission();
        // The last-resort local fence retains the closure/lease guards and
        // journal, but cannot recurse into the failed checkpoint path.
        let _ = self.with_sealed_tx_fenced(
            || self.write_fence.get().and_then(|f| f.check()),
            |tx| {
                require_clean_journal_in(tx)?;
                tx.execute_raw(
                    "UPDATE connection_dispatch_config SET fenced=1 WHERE singleton=1",
                    [],
                )
                .map_err(sql_error)?;
                let _ = seal_authority_changes_in(tx)?;
                Ok(())
            },
        );
        Error::OutcomeUnknown(format!(
            "the protected authorizing outcome is unknown; its external checkpoint is unacknowledged: {}",
            error.code().unwrap_or("storage_unavailable")
        ))
    }

    pub(crate) fn protected_quote(&self, quote_id: &str) -> Result<Option<ModelQuoteV1>> {
        self.read_tx(|conn| quote_row_in(&*conn, quote_id))
    }

    /// The process activation a quote was observed under. A restart mints a
    /// new activation, so a pre-restart quote is never usable.
    pub(crate) fn protected_quote_activation(&self, quote_id: &str) -> Result<Option<String>> {
        self.read_tx(|conn| {
            Ok(conn
                .query_opt(
                    "SELECT process_activation FROM connection_model_quotes WHERE quote_id=?1",
                    [quote_id],
                    |row| row.get(0),
                )?
                .flatten())
        })
    }

    pub(crate) fn protected_connection_row(
        &self,
        connection_id: &str,
    ) -> Result<Option<ConnectionRow>> {
        self.read_tx(|conn| connection_row_in(&*conn, connection_id))
    }

    pub(crate) fn protected_policy_row(&self, connection_id: &str) -> Result<Option<PolicyRow>> {
        self.read_tx(|conn| policy_row_in(&*conn, connection_id))
    }

    pub(crate) fn protected_grants(
        &self,
        agent_uid: &str,
        platform: &str,
        account: &str,
    ) -> Result<Vec<UidGrantV1>> {
        self.read_tx(|conn| grants_in(&*conn, agent_uid, platform, account))
    }

    pub(crate) fn protected_role_model_policy(
        &self,
    ) -> Result<Option<cadence_core::connection_dispatch::RoleModelPolicyV1>> {
        self.read_tx(|conn| role_model_policy_in(&*conn))
    }

    pub(crate) fn protected_put_quote(
        &self,
        quote: ModelQuoteV1,
        principal_key: String,
        process_activation: String,
    ) -> Result<()> {
        self.write_tx(move |tx| put_quote_in(tx, &quote, &principal_key, &process_activation))
    }

    pub(crate) fn protected_manifest(&self, manifest_id: &str) -> Result<Option<ManifestV1>> {
        self.read_tx(|conn| Ok(manifest_row_in(&*conn, manifest_id)?.map(|(row, _)| row)))
    }

    /// The stable agent UID recorded for one alias, if any. An absent/legacy
    /// row answers `None` and never authorizes protected use.
    pub(crate) fn agent_uid_for_alias(&self, alias: &str) -> Result<Option<String>> {
        self.read_tx(|conn| {
            Ok(conn
                .query_opt("SELECT uid FROM agents WHERE alias=?1", [alias], |row| {
                    row.get::<_, Option<String>>(0)
                })?
                .flatten())
        })
    }

    /// Re-prove the CURRENT authority before a cached protected result is
    /// returned: enabled/unfenced store, an active reviewed policy and — for
    /// an agent operation — exactly one stamped grant for this stable UID on
    /// this exact Connection account. Reads only; never mints a permit.
    pub(crate) fn protected_read_authorized(
        &self,
        principal_key: &str,
        agent_uid: Option<&str>,
        connection_id: &str,
    ) -> Result<()> {
        let _ = principal_key;
        self.read_tx(|conn| {
            let authority = authority_in(&*conn)?;
            if self.admission_fenced() || !authority.enabled || authority.fenced {
                return Err(Error::invalid(
                    "runtime_revoked",
                    "the runtime is fenced or revoked",
                ));
            }
            let connection = connection_row_in(&*conn, connection_id)?
                .ok_or_else(|| Error::invalid("not_found", "the selector is unavailable"))?;
            let policy = policy_row_in(&*conn, connection_id)?.ok_or_else(|| {
                Error::invalid(
                    "operation_not_granted",
                    "the Connection has no reviewed protected policy",
                )
            })?;
            if policy.state != "active" || !policy.policy.active {
                return Err(Error::invalid(
                    "operation_not_granted",
                    "the Connection does not grant this operation",
                ));
            }
            if let Some(uid) = agent_uid {
                let grants = grants_in(&*conn, uid, &connection.platform, &connection.account)?;
                if grants.len() != 1 {
                    return Err(Error::invalid(
                        "operation_not_granted",
                        "the Connection does not grant this operation",
                    ));
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "native_bookkeeping_authority_acceptance.rs"]
mod native_bookkeeping_authority_acceptance;

#[cfg(test)]
#[path = "checkpoint_chunks_acceptance_20261027.rs"]
mod checkpoint_chunks_acceptance_20261027;

#[cfg(test)]
#[path = "checkpoint_ordering_acceptance_20261027.rs"]
mod checkpoint_ordering_acceptance_20261027;

#[cfg(test)]
#[path = "protected_side_store_acceptance_20261027.rs"]
mod protected_side_store_acceptance_20261027;
