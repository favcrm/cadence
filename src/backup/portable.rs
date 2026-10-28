//! CAD-1163: generic portable coordination package (export and inactive restore).
//!
//! Export takes a private online snapshot of the store, projects it to the
//! coordination profile on that private copy only, runs the existing backup
//! scrub/scan/integrity checks on it, copies a pinned clean tracker commit,
//! and publishes the staged package with a no-replace rename only after
//! every member and the manifest are written. The source store and tracker
//! are only read and never written.
//!
//! Restore verifies every manifest member before writing, stages the core
//! store and tracker privately, and commits inactive: no actor is enabled,
//! no session or dispatch starts, and no default changes. Replacement and
//! activation remain refused until native operator admission is wired.

#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    artifact_snapshot, inspect, quote_ident, require_ok, scan_db, scrub, sync_dir, ArtifactDir,
};
use crate::error::{Error, Result};
use crate::rollout::db_file;
use crate::secret::Allowlist;

mod capture;
#[cfg(test)]
mod capture_acceptance;
#[cfg(test)]
mod hook_capture_acceptance;

pub const PORTABLE_FORMAT: &str = "cadence.portable/1";
pub const INACTIVE_MARKER: &str = "portable-restore.json";
const PKG_DB: &str = "cadence.sqlite3";
const PKG_MANIFEST: &str = "manifest.json";
const PKG_TRACKER: &str = "tracker";
/// Where the restored tracker lives inside a destination state dir.
const TRACKER_DIR: &str = "portable-tracker";

/// Coordinator data carried by the coordination profile.
const COORDINATION: &[&str] = &[
    "schema_version",
    "agents",
    "messages",
    "events",
    "jobs",
    "tasks",
    "verdicts",
    "threads",
    "thread_entries",
    "monitors",
    "monitor_tasks",
    "monitor_alerts",
    "topics",
    "topic_subscriptions",
    "topic_posts",
    "topic_deliveries",
];

/// Retained-template delete guards. They refuse DELETE on the store, so
/// `project` drops these two on the private copy only, inside its own
/// transaction, and restores their stored SQL before commit.
const PRIVATE_DELETE_GUARDS: &[&str] = &[
    "business_template_source_no_delete",
    "business_template_approved_no_delete",
];

/// Live authority of the source host: nulled on the private copy.
const NULLED: &[(&str, &str)] = &[
    ("agents", "generation"),
    ("agents", "pid"),
    ("agents", "pid_start"),
    ("agents", "endpoint"),
    ("agents", "thread_id"),
    ("agents", "session_id"),
    ("agents", "params"),
    ("messages", "turn_id"),
];

/// Imported actors and dispatch flags: forced to 0 on the private copy.
const DISABLED: &[(&str, &str)] = &[
    ("agents", "enabled"),
    ("monitors", "dispatch_enabled"),
    ("monitors", "auto_dispatch_enabled"),
];

/// The fixed catalog of the store tables with their exact columns.
/// Export and restore refuse any table or column not listed here.
const CATALOG: &[(&str, &[&str])] = &[
    (
        "agents",
        &[
            "alias",
            "provider",
            "endpoint_kind",
            "role",
            "cwd",
            "sandbox",
            "instructions",
            "thread_id",
            "session_id",
            "model",
            "effort",
            "pid",
            "state",
            "enabled",
            "gc_keep",
            "archived_at",
            "archived_by",
            "archive_reason",
            "lifecycle_rev",
            "error",
            "created",
            "updated",
            "endpoint",
            "params",
            "generation",
            "quota",
            "team_role",
            "model_selection",
            "pid_start",
            "uid",
        ],
    ),
    ("app_media_asset_origins", &["asset_id", "receipt_id"]),
    (
        "app_media_assets",
        &[
            "id",
            "install_id",
            "title",
            "kind",
            "lifecycle",
            "revision",
            "creator",
            "created",
            "updated",
        ],
    ),
    (
        "app_media_publish_requests",
        &[
            "request_id",
            "asset_id",
            "expected_revision",
            "result",
            "created",
        ],
    ),
    (
        "app_media_lineage",
        &["asset_id", "source_asset_id", "source_sha256"],
    ),
    (
        "app_bindings",
        &[
            "id",
            "install_id",
            "context_id",
            "scope_key",
            "slot",
            "revision",
            "state",
            "config",
            "digest",
            "request_id",
            "created",
            "updated",
        ],
    ),
    (
        "app_capability_claims",
        &[
            "run_id",
            "slot",
            "step_id",
            "request_id",
            "binding_digest",
            "input_digest",
            "call_id",
            "created",
        ],
    ),
    (
        "app_capability_epochs",
        &["install_id", "epoch", "digest", "state", "created"],
    ),
    (
        "app_capability_results",
        &[
            "id",
            "run_id",
            "step_id",
            "message_id",
            "turn_id",
            "slot",
            "request_id",
            "binding_digest",
            "input_digest",
            "result",
            "result_digest",
            "asset_type",
            "asset_digest",
            "asset",
            "created",
            "receipt_schema",
        ],
    ),
    (
        "app_contexts",
        &[
            "id",
            "install_id",
            "request_id",
            "revision",
            "state",
            "config_json",
            "config_digest",
            "created",
            "updated",
        ],
    ),
    (
        "app_effect_authorizations",
        &[
            "effect_id",
            "install_id",
            "context_id",
            "run_id",
            "artifact_id",
            "authority",
            "authority_digest",
            "input_digest",
            "release_digest",
        ],
    ),
    (
        "app_favorites",
        &["owner", "install_id", "position", "opened_at", "saved"],
    ),
    (
        "app_grants",
        &[
            "app",
            "agent",
            "platform",
            "account",
            "scopes",
            "granted_at",
            "by",
            "install_id",
        ],
    ),
    (
        "app_install_capabilities",
        &["install_id", "epoch", "digest", "state", "created"],
    ),
    (
        "app_install_requests",
        &["id", "catalog_id", "requested_by", "at", "state"],
    ),
    (
        "app_run_artifact_fetches",
        &["artifact_id", "message_id", "turn_hash", "created"],
    ),
    (
        "app_run_artifacts",
        &[
            "id",
            "run_id",
            "step_id",
            "message_id",
            "turn_id",
            "producer",
            "digest",
            "media_type",
            "content",
            "created",
        ],
    ),
    (
        "app_run_dispositions",
        &[
            "run_id",
            "request_id",
            "artifact_id",
            "artifact_digest",
            "decision",
            "created",
        ],
    ),
    (
        "app_run_failures",
        &["run_id", "step_id", "kind", "reason", "created"],
    ),
    (
        "app_run_reviews",
        &[
            "run_id",
            "step_id",
            "artifact_id",
            "artifact_digest",
            "reviewer",
            "message_id",
            "decision",
            "rationale",
            "asset_receipt_id",
            "asset_digest",
        ],
    ),
    (
        "app_run_steps",
        &[
            "run_id",
            "step_id",
            "task_id",
            "spec",
            "identity_digest",
            "state",
            "message_id",
            "result_digest",
        ],
    ),
    (
        "app_run_tickets",
        &[
            "run_id",
            "request_id",
            "artifact_id",
            "artifact_digest",
            "suggestion_index",
            "suggestion_count",
            "suggestion_text",
            "issue_id",
            "created",
        ],
    ),
    (
        "app_runs",
        &[
            "id",
            "install_id",
            "epoch",
            "bundle_digest",
            "snapshot",
            "snapshot_digest",
            "project_link",
            "owner_pm",
            "request_id",
            "state",
            "approved_digest",
            "created",
            "updated",
            "context_id",
        ],
    ),
    (
        "app_tool_claims",
        &[
            "request_id",
            "install_id",
            "alias",
            "slot",
            "binding_digest",
            "input_digest",
            "call_id",
            "created",
        ],
    ),
    (
        "app_tool_results",
        &[
            "id",
            "request_id",
            "install_id",
            "alias",
            "slot",
            "binding_digest",
            "input_digest",
            "input",
            "result",
            "result_digest",
            "asset_type",
            "asset_digest",
            "asset",
            "created",
        ],
    ),
    (
        "app_update_checks",
        &[
            "install_id",
            "checked_at",
            "proposal_json",
            "has_update",
            "version",
            "access_change_json",
            "digest",
        ],
    ),
    ("app_update_preferences", &["install_id", "automatic"]),
    (
        "artifacts",
        &[
            "id",
            "version",
            "kind",
            "title",
            "bytes",
            "digest",
            "scope",
            "context_id",
            "thread_id",
            "created_by",
            "created",
        ],
    ),
    ("assist_job_shares", &["job", "org", "shared_by", "created"]),
    (
        "assist_requests",
        &[
            "org",
            "sub",
            "request_key",
            "job",
            "created",
            "title",
            "body",
            "acceptance",
            "credential_id",
            "authority_rev",
            "state",
        ],
    ),
    (
        "assist_stall_checks",
        &["job", "revision", "state", "created", "resolved"],
    ),
    (
        "assistants",
        &[
            "org",
            "sub",
            "assistant_id",
            "alias",
            "conversation_id",
            "state",
            "authority_rev",
            "created",
            "updated",
            "bootstrap_state",
            "bootstrap_template_version",
            "bootstrap_revision",
        ],
    ),
    (
        "business_documents",
        &[
            "company_id",
            "id",
            "template_name",
            "template_version",
            "template_sha256",
            "kind",
            "input_json",
            "context_json",
            "created",
        ],
    ),
    (
        "business_template_sources",
        &["company_id", "sha256", "size", "bytes", "created"],
    ),
    (
        "business_template_versions",
        &[
            "company_id",
            "name",
            "version",
            "based_on",
            "status",
            "kind",
            "spec_json",
            "content_sha256",
            "created",
            "approved_source",
            "approved_at",
            "source_sha256",
        ],
    ),
    (
        "chat_files",
        &[
            "id",
            "sha256",
            "size",
            "mime",
            "name",
            "scope",
            "context_id",
            "uploader",
            "created",
            "user_id",
            "thread_id",
            "location_version",
        ],
    ),
    (
        "chat_upload_accounts",
        &["id", "principal_hash", "authority", "created"],
    ),
    (
        "cloud_dispatch_outbox",
        &[
            "cursor",
            "message_id",
            "source",
            "task_id",
            "task_revision",
            "audience_agent",
            "expected_head",
            "payload_digest",
            "organization_id",
            "remote_turn_id",
            "created",
            "claimed",
            "cloud_eligible",
        ],
    ),
    (
        "cloud_agent_operations",
        &[
            "operation_id",
            "cloud_agent_id",
            "source_kind",
            "source_uid",
            "registration_epoch",
            "owner_issuer",
            "owner_subject",
            "recipient_uid",
            "sender_label",
            "thread_id",
            "conversation_id",
            "remote_session_id",
            "request_digest",
            "request_text",
            "status",
            "remote_operation_id",
            "message_id",
            "reply_text",
            "reply_message_id",
            "error_code",
            "error_message",
            "created",
            "updated",
            "grant_admitted_at",
            "claimed_at",
        ],
    ),
    (
        "cloud_agent_sessions",
        &[
            "id",
            "cloud_agent_id",
            "source_kind",
            "source_uid",
            "registration_epoch",
            "owner_issuer",
            "owner_subject",
            "thread_id",
            "conversation_id",
            "remote_session_id",
            "provider_session_id",
            "invoker_id",
            "created",
        ],
    ),
    (
        "cloud_agent_sources",
        &[
            "cloud_agent_id",
            "source_kind",
            "source_uid",
            "registration_epoch",
            "owner_issuer",
            "owner_subject",
            "label",
            "scope",
            "granted_by",
            "granted_at",
            "invoker_id",
            "state",
        ],
    ),
    (
        "cloud_agents",
        &[
            "id",
            "alias",
            "organization_id",
            "org_slug",
            "endpoint",
            "remote_agent_id",
            "runtime",
            "model",
            "capabilities",
            "provider_session_id",
            "provenance",
            "recipient_uid",
            "consent_scope",
            "state",
            "created",
            "updated",
        ],
    ),
    ("connection_metadata", &["singleton", "workspace_id"]),
    (
        "crm_sends",
        &[
            "install_id",
            "context_id",
            "send_id",
            "state",
            "created",
            "updated",
        ],
    ),
    ("crm_settings", &["key", "value", "by", "updated"]),
    (
        "crm_smtp_links",
        &[
            "install_id",
            "context_id",
            "connection_id",
            "auth_revision",
            "link_revision",
            "state",
            "digest",
            "request_id",
            "created",
            "updated",
        ],
    ),
    (
        "crm_unsubscribe_index",
        &["token_hash", "install_id", "context_id"],
    ),
    ("daemon_build", &["id", "commit_sha", "recorded_at"]),
    (
        "events",
        &["seq", "alias", "kind", "payload", "at", "job_id", "task_id"],
    ),
    (
        "job_subscribers",
        &["job", "org", "sub", "assistant_id", "created"],
    ),
    (
        "jobs",
        &[
            "id",
            "title",
            "spec_path",
            "spec_sha256",
            "pm_alias",
            "issue_id",
            "repo",
            "base_ref",
            "state",
            "max_revisions",
            "error",
            "created",
            "updated",
            "stall_secs",
        ],
    ),
    (
        "messages",
        &[
            "seq",
            "id",
            "alias",
            "body",
            "reply_to",
            "source",
            "state",
            "turn_id",
            "result",
            "error",
            "created",
            "started",
            "completed",
            "task_id",
            "priority",
            "issue",
            "worktree",
            "sender_uid",
            "recipient_uid",
            "sender",
            "recipient",
            "origin",
            "stall_secs",
        ],
    ),
    ("model_defaults", &["id", "revision", "document"]),
    (
        "monitor_alerts",
        &[
            "seq",
            "monitor_id",
            "task_id",
            "event_seq",
            "fingerprint",
            "kind",
            "payload",
            "state",
            "attempts",
            "last_error",
            "created",
            "updated",
        ],
    ),
    ("monitor_tasks", &["monitor_id", "task_id"]),
    (
        "monitors",
        &[
            "id",
            "project",
            "owner",
            "interval_secs",
            "state",
            "heartbeat_at",
            "last_check_at",
            "last_success_at",
            "next_check_at",
            "event_cursor",
            "delivery_configured",
            "delivery_state",
            "dispatch_enabled",
            "error",
            "created",
            "updated",
            "auto_dispatch_enabled",
        ],
    ),
    (
        "platform_credentials",
        &[
            "platform",
            "account",
            "scopes",
            "fingerprint",
            "custody",
            "exchange",
            "enrolled_at",
            "by",
            "connection_id",
            "credential_revision",
        ],
    ),
    (
        "platform_defaults",
        &["project", "platform", "account", "set_at", "by"],
    ),
    (
        "platform_drafts",
        &[
            "id",
            "agent",
            "platform",
            "account",
            "tool",
            "label",
            "input_summary",
            "artifact",
            "ran_at",
        ],
    ),
    (
        "platform_effects",
        &[
            "effect_id",
            "request",
            "agent",
            "platform",
            "account",
            "tool",
            "label",
            "input",
            "input_summary",
            "preview",
            "source_name",
            "source_hash",
            "scopes",
            "task",
            "state",
            "close_reason",
            "decision",
            "outcome",
            "needs_you",
            "staged_at",
            "updated_at",
            "authorization_kind",
        ],
    ),
    (
        "platform_grants",
        &["agent", "platform", "account", "scopes", "granted_at", "by"],
    ),
    (
        "rollout_grants",
        &[
            "id",
            "alias",
            "granted_by",
            "granted_at",
            "expires_at",
            "revoked_at",
            "revoked_by",
        ],
    ),
    (
        "rollout_leases",
        &[
            "id",
            "holder",
            "holder_source",
            "host_note",
            "reason",
            "target_commit",
            "schema_from",
            "schema_to",
            "backup_path",
            "backup_sha256",
            "backup_schema",
            "backup_taken_at",
            "claimed_at",
            "expires_at",
            "status",
            "previous_holder",
            "ended_at",
            "end_reason",
        ],
    ),
    ("schema_version", &["version"]),
    (
        "settings_authority",
        &["id", "pi", "profiles", "legacy_imported"],
    ),
    (
        "social_publish_intents",
        &[
            "intent_id",
            "request",
            "install_id",
            "context_id",
            "run_id",
            "effect_id",
            "connection_id",
            "destination_id",
            "toolkit",
            "caption_digest",
            "image_digest",
            "media_key",
            "grant_id",
            "approval_id",
            "due_epoch",
            "timezone",
            "state",
            "frozen",
            "frozen_digest",
            "receipt",
            "upstream",
            "created",
            "updated",
        ],
    ),
    (
        "staging_grants",
        &[
            "id",
            "alias",
            "ops",
            "state_dir",
            "granted_by",
            "granted_at",
            "expires_at",
            "revoked_at",
            "revoked_by",
            "last_used_at",
        ],
    ),
    (
        "staging_instances",
        &[
            "id",
            "state_dir",
            "board_port",
            "registered_by",
            "registered_at",
        ],
    ),
    (
        "human_principals",
        &["id", "org", "issuer", "subject", "name", "seen"],
    ),
    ("human_read_markers", &["room", "human", "seq"]),
    (
        "human_notification_prefs",
        &[
            "human", "dm", "mention", "sound", "preview", "quiet", "zone", "updated",
        ],
    ),
    ("human_notification_reads", &["room", "human", "seq"]),
    ("human_room_mutes", &["room", "human", "muted"]),
    (
        "human_push_devices",
        &[
            "id", "org", "issuer", "subject", "human", "endpoint", "p256dh", "auth", "session",
            "enrolled", "expires", "revoked",
        ],
    ),
    (
        "human_push_deliveries",
        &[
            "intent",
            "device",
            "state",
            "attempts",
            "next_at",
            "lease",
            "lease_until",
        ],
    ),
    ("human_push_presence", &["device", "tab", "room", "expires"]),
    (
        "human_notification_intents",
        &[
            "id",
            "room",
            "post",
            "seq",
            "recipient",
            "reason",
            "attempts",
            "created",
        ],
    ),
    (
        "human_room_members",
        &["room", "kind", "member", "cutoff", "added", "removed"],
    ),
    (
        "human_rooms",
        &[
            "id", "topic_id", "org", "kind", "pair", "title", "creator", "created",
        ],
    ),
    (
        "tasks",
        &[
            "id",
            "job_id",
            "title",
            "role",
            "assignee",
            "spec_path",
            "acceptance",
            "worktree",
            "branch",
            "base_sha",
            "head_sha",
            "state",
            "revision",
            "dispatch_message",
            "error",
            "created",
            "updated",
        ],
    ),
    (
        "thread_entries",
        &[
            "seq",
            "thread_id",
            "role",
            "kind",
            "text",
            "payload",
            "message_id",
            "created",
        ],
    ),
    (
        "threads",
        &[
            "id",
            "alias",
            "archived_alias",
            "created",
            "updated",
            "install_id",
            "context_id",
            "subject",
            "is_general",
            "archived",
            "title",
        ],
    ),
    (
        "topic_deliveries",
        &["post_id", "uid", "alias", "message_id", "state"],
    ),
    (
        "topic_posts",
        &[
            "seq",
            "id",
            "topic_id",
            "sender_uid",
            "sender",
            "body",
            "created",
            "request_message",
        ],
    ),
    ("topic_subscriptions", &["topic_id", "uid", "added"]),
    ("topics", &["id", "type", "source", "created"]),
    (
        "verdicts",
        &[
            "seq", "task_id", "revision", "sha", "verdict", "reviewer", "evidence", "message",
            "created", "verify",
        ],
    ),
    (
        "wiki_transfer_entries",
        &[
            "job_id",
            "entry_id",
            "ordinal",
            "path",
            "rel_path",
            "kind",
            "size",
            "sha256",
            "if_rev",
            "received",
            "outcome",
            "rev",
            "extraction",
            "extraction_rev",
            "source_hash",
            "detail",
        ],
    ),
    (
        "wiki_transfer_jobs",
        &[
            "job_id",
            "digest",
            "manifest_json",
            "source_id",
            "root",
            "owner_key",
            "state",
            "root_attempt",
            "index_kicked",
            "created_at",
        ],
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Coordinator data and the pinned tracker. Not a full workspace.
    Coordination,
}

#[derive(Clone, Debug)]
pub struct PortableExportOptions {
    pub scope: Scope,
    /// The ordinary Git tracker checkout to pin into the package.
    pub tracker: PathBuf,
    /// Redact credential-shaped spans in private-copy message bodies.
    pub redact_history_secrets: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PortableManifest {
    pub format: String,
    pub scope: String,
    pub full_workspace: bool,
    pub created_at: String,
    /// Source workspace UUID: non-authoritative provenance, never the
    /// destination identity.
    pub source_uuid: Option<String>,
    pub cadence: String,
    pub schema_version: i64,
    pub tracker_revision: String,
    /// PM root inside the tracker: `.` for `pm.yaml` at the root, `pm` for a
    /// nested PM dir, `None` when the tracker holds no `pm.yaml`. Recorded
    /// at export; restore re-validates the staged copy with `Pm::at`.
    #[serde(default)]
    pub pm_root: Option<String>,
    pub tables: Vec<String>,
    pub omitted_tables: Vec<String>,
    /// Rows deleted from the private copy per omitted table.
    pub omitted_rows: Vec<(String, i64)>,
    pub nulled_columns: Vec<String>,
    pub disabled_columns: Vec<String>,
    pub exclusions: Vec<String>,
    /// None means strict history export; Some(0) still records explicit opt-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_message_bodies: Option<u64>,
    pub members: Vec<Member>,
}

/// Restore options. `destination` is the destination state dir (already a
/// PM path for the coordinator), passed explicitly; nothing is inferred.
#[derive(Default, Debug)]
pub struct PortableRestoreOptions {
    /// Replacement needs native operator admission; refused in this slice.
    pub replace: bool,
}

/// Native operator proof for activation (not wired). Defined in
/// `portable_replace`; re-exported so the activation signature stays.
pub use super::portable_replace::NativeOperatorProof;

// ---------- export ----------

/// Write a portable coordination package into the new directory `out`.
pub fn export_portable(
    state_dir: &Path,
    out: &Path,
    opts: &PortableExportOptions,
) -> Result<Value> {
    let custody_paths = super::refuse_custody_output(state_dir, out)?;
    if let Some(config) = &custody_paths {
        config.refuse_output(&opts.tracker)?;
    }
    let Scope::Coordination = opts.scope;
    let live = db_file(state_dir);
    if !live.is_file() {
        return Err(Error::rejected(format!(
            "no cadence database at {}",
            live.display()
        )));
    }
    refuse_existing(out)?;
    let allow = Allowlist::load(state_dir)?;
    let head = admit_tracker(&opts.tracker)?;
    let staging = private_staging(out)?;
    match build_package(
        &live,
        &staging,
        &opts.tracker,
        &head,
        &allow,
        opts.redact_history_secrets,
    ) {
        Ok(manifest) => publish(&staging, out, &manifest),
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

/// `export_portable` with an explicit app selection. No app schema is
/// supported by the coordination format yet, so any selected app refuses
/// before the store, tracker or output path is touched. Names are opaque
/// diagnostics, quoted and never interpreted as paths. An empty selection is
/// the coordination-only export.
pub fn export_portable_selected(
    state_dir: &Path,
    out: &Path,
    opts: &PortableExportOptions,
    apps: &[String],
) -> Result<Value> {
    if let Some(app) = apps.first() {
        return Err(Error::rejected(format!(
            "app {app:?} has no supported portable schema; the coordination format exports no app data, so nothing was written"
        )));
    }
    export_portable(state_dir, out, opts)
}

/// Capture a dirty working tree privately, then use the same native package
/// builder. Publication follows independent package validation and a final
/// source check; the source database and tracker are not one atomic snapshot.
pub fn export_portable_snapshot_selected(
    state_dir: &Path,
    out: &Path,
    opts: &PortableExportOptions,
    apps: &[String],
) -> Result<Value> {
    if !apps.is_empty() {
        return export_portable_selected(state_dir, out, opts, apps);
    }
    let custody_paths = super::refuse_custody_output(state_dir, out)?;
    if let Some(config) = &custody_paths {
        config.refuse_output(&opts.tracker)?;
    }
    refuse_existing(out)?;
    refuse_output_in_tracker(out, &opts.tracker)?;
    let live = db_file(state_dir);
    if !live.is_file() {
        return Err(Error::rejected(
            "portable export source database is missing",
        ));
    }
    let allow = Allowlist::load(state_dir)?;
    let captured = capture::TrackerCapture::new(&opts.tracker)?;
    let head = admit_tracker(captured.path())?;
    let staging = private_staging(out)?;
    let result = (|| {
        let manifest = build_package(
            &live,
            &staging,
            captured.path(),
            &head,
            &allow,
            opts.redact_history_secrets,
        )?;
        let mut report = inspect_portable(&staging)?;
        captured.verify_package(&staging, &manifest.members)?;
        captured.verify_unchanged()?;
        report["source_capture"] = captured.evidence();
        report["exporter_revision"] = json!(crate::overview::BUILD_COMMIT);
        report["source_fidelity"] = json!("captured_tracker_matches_package_and_rechecked");
        report["atomic_database_and_tracker"] = json!(false);
        report["bundle"] = json!(out);
        rename_noreplace(&staging, out)?;
        Ok(report)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn refuse_output_in_tracker(out: &Path, tracker: &Path) -> Result<()> {
    let absolute = std::env::current_dir()?.join(out);
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    while ancestor.symlink_metadata().is_err() {
        suffix.push(
            ancestor
                .file_name()
                .ok_or_else(|| Error::rejected("output path has no resolvable parent"))?,
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Error::rejected("output path has no resolvable parent"))?;
    }
    let mut resolved = ancestor.canonicalize()?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    if resolved.starts_with(tracker.canonicalize()?) {
        return Err(Error::rejected(
            "snapshot export output must be outside the source tracker",
        ));
    }
    Ok(())
}

/// Read-only inspection through the same integrity, catalog and authority
/// checks as inactive restore. A valid manifest is not source authenticity,
/// present-day source fidelity, or destination/import admission.
/// A portable package that passed every inspection check: the verified
/// manifest, the exact manifest bytes and their sha256, and the inspection
/// report `inspect-portable` prints. Transfer callers bind the digest of
/// these exact bytes — the file is hashed once, never re-read.
pub struct VerifiedPackage {
    pub manifest: PortableManifest,
    pub manifest_bytes: Vec<u8>,
    pub manifest_sha256: String,
    pub report: Value,
}

/// `inspect_portable` plus the verified package internals a remote transfer
/// needs (the exact manifest bytes and member list). One verification pass.
pub fn verify_transfer_package(source: &Path) -> Result<VerifiedPackage> {
    let inspected = inspect_package(source)?;
    if inspected.manifest_bytes.len() > TRANSFER_MANIFEST_MAX {
        return Err(Error::rejected(
            "package manifest exceeds the transfer limit",
        ));
    }
    if inspected.manifest.members.len() > TRANSFER_MEMBERS_MAX {
        return Err(Error::rejected(
            "package member count exceeds the transfer limit",
        ));
    }
    admit_transfer_manifest(&inspected.manifest_bytes, &inspected.manifest.members)?;
    Ok(inspected)
}

/// Transfer-only admission that mirrors the hosted manifest parser's closed
/// schema and inventory bounds, so the client never offers a package the
/// server always refuses. Pure over the manifest bytes: nothing here reads a
/// member file. Shared local-restore verification is unchanged.
fn admit_transfer_manifest(manifest_bytes: &[u8], members: &[Member]) -> Result<()> {
    let raw: Value = serde_json::from_slice(manifest_bytes)
        .map_err(|_| Error::rejected("package manifest is not valid JSON"))?;
    let top = raw
        .as_object()
        .ok_or_else(|| Error::rejected("package manifest is not a JSON object"))?;
    if top
        .keys()
        .any(|key| !TRANSFER_MANIFEST_KEYS.contains(&key.as_str()))
    {
        return Err(Error::rejected(
            "package manifest has a key the transfer schema does not admit",
        ));
    }
    if let Some(raw_members) = top.get("members").and_then(Value::as_array) {
        for raw_member in raw_members {
            let closed = raw_member.as_object().is_some_and(|object| {
                object.len() == 3
                    && ["path", "bytes", "sha256"]
                        .iter()
                        .all(|key| object.contains_key(*key))
            });
            if !closed {
                return Err(Error::rejected(
                    "package member has a key the transfer schema does not admit",
                ));
            }
        }
    }
    let inventory_bytes = transfer_inventory_bytes(manifest_bytes, members)?;
    for member in members {
        if member.path.encode_utf16().count() > TRANSFER_MEMBER_PATH_MAX_UNITS {
            return Err(Error::rejected(
                "package member path exceeds the transfer limit",
            ));
        }
        if member.bytes > TRANSFER_MEMBER_MAX_BYTES {
            return Err(Error::rejected(
                "package member exceeds the transfer size limit",
            ));
        }
    }
    if inventory_bytes > TRANSFER_INVENTORY_MAX_BYTES {
        return Err(Error::rejected(
            "package inventory exceeds the transfer size limit",
        ));
    }
    Ok(())
}

/// Hosted inventory total: declared member bytes plus the manifest's own bytes
/// (the sealed inventory adds `manifest.json` as one more member). The single
/// source for both transfer admission and the preflight comparison, so the
/// client refuses exactly what the server refuses. Errors on overflow.
pub fn transfer_inventory_bytes(manifest_bytes: &[u8], members: &[Member]) -> Result<u64> {
    members
        .iter()
        .try_fold(manifest_bytes.len() as u64, |sum, m| {
            sum.checked_add(m.bytes)
        })
        .ok_or_else(|| Error::rejected("package inventory exceeds the transfer size limit"))
}

#[cfg(test)]
mod transfer_admission_tests {
    use super::*;

    fn manifest_with(members: &str) -> Vec<u8> {
        format!(
            "{{\"format\":\"cadence.portable/1\",\"scope\":\"x\",\"full_workspace\":false,\"created_at\":\"2026-01-01T00:00:00Z\",\"source_uuid\":\"u\",\"cadence\":\"1\",\"schema_version\":1,\"tracker_revision\":\"r\",\"pm_root\":null,\"tables\":[],\"omitted_tables\":[],\"omitted_rows\":[],\"nulled_columns\":[],\"disabled_columns\":[],\"exclusions\":[],\"redacted_message_bodies\":null,\"members\":{members}}}"
        )
        .into_bytes()
    }

    fn member(path: &str, bytes: u64) -> Member {
        Member {
            path: path.into(),
            bytes,
            sha256: "a".repeat(64),
        }
    }

    #[test]
    fn extra_member_key_is_refused_without_echoing_it() {
        let bytes =
            manifest_with("[{\"path\":\"a\",\"bytes\":1,\"sha256\":\"aa\",\"note\":\"secret-x\"}]");
        let err = admit_transfer_manifest(&bytes, &[member("a", 1)]).unwrap_err();
        let text = format!("{err:?}");
        assert!(text.contains("transfer schema does not admit"));
        assert!(!text.contains("secret-x"));
    }

    #[test]
    fn closed_member_object_is_admitted() {
        let bytes = manifest_with("[{\"path\":\"a\",\"bytes\":1,\"sha256\":\"aa\"}]");
        assert!(admit_transfer_manifest(&bytes, &[member("a", 1)]).is_ok());
    }

    #[test]
    fn long_member_path_is_refused() {
        let bytes = manifest_with("[]");
        let long = "x".repeat(TRANSFER_MEMBER_PATH_MAX_UNITS + 1);
        assert!(admit_transfer_manifest(&bytes, &[member(&long, 1)]).is_err());
    }

    #[test]
    fn oversized_member_is_refused() {
        let bytes = manifest_with("[]");
        let over = TRANSFER_MEMBER_MAX_BYTES + 1;
        assert!(admit_transfer_manifest(&bytes, &[member("a", over)]).is_err());
    }

    #[test]
    fn inventory_total_includes_manifest_bytes_and_is_capped() {
        let bytes = manifest_with("[]");
        // Members sum to exactly the cap, but the manifest's own bytes push it over.
        let members_sum = TRANSFER_INVENTORY_MAX_BYTES - bytes.len() as u64 + 1;
        let total = transfer_inventory_bytes(&bytes, &[member("a", members_sum)]).unwrap();
        assert_eq!(total, TRANSFER_INVENTORY_MAX_BYTES + 1);
        assert!(admit_transfer_manifest(&bytes, &[member("a", members_sum)]).is_err());
    }
}
/// The 17 top-level keys the hosted manifest parser admits.
const TRANSFER_MANIFEST_KEYS: &[&str] = &[
    "format",
    "scope",
    "full_workspace",
    "created_at",
    "source_uuid",
    "cadence",
    "schema_version",
    "tracker_revision",
    "pm_root",
    "tables",
    "omitted_tables",
    "omitted_rows",
    "nulled_columns",
    "disabled_columns",
    "exclusions",
    "redacted_message_bodies",
    "members",
];
/// Hosted member path bound, counted in UTF-16 code units (JavaScript length).
pub const TRANSFER_MEMBER_PATH_MAX_UNITS: usize = 512;
/// Hosted per-member byte bound (512 MiB).
pub const TRANSFER_MEMBER_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// Hosted inventory total bound (2 GiB): members plus the manifest's own bytes.
pub const TRANSFER_INVENTORY_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Transfer bounds the hosted packed-v1 endpoint enforces. The CLI refuses a
/// package outside them before any byte is sent; the server stays the
/// enforcement of record.
pub const TRANSFER_MANIFEST_MAX: usize = 4 * 1024 * 1024;
/// Logical data members cap (19,998). The sealed inventory adds `manifest.json`
/// as one separate entry (19,999 total), which is not counted here. The hosted
/// manifest parser admits 19,998 declared members, so this client-side refusal
/// matches the server bound instead of admitting what the server rejects.
pub const TRANSFER_MEMBERS_MAX: usize = 19998;

fn inspect_package(source: &Path) -> Result<VerifiedPackage> {
    if !source.symlink_metadata()?.is_dir() {
        return Err(Error::rejected("package is not a plain directory"));
    }
    require_regular(&source.join(PKG_MANIFEST))?;
    // Reject linked directories before reading any listed member through them.
    let mut paths = Vec::new();
    collect_package_members(source, source, &mut paths)?;
    let manifest_bytes = fs::read(source.join(PKG_MANIFEST))?;
    let manifest_sha256 = format!("{:x}", Sha256::digest(&manifest_bytes));
    let manifest = verify_manifest_bytes(source, &manifest_bytes)?;
    // Ordinary read-only SQLite opens can still create WAL/SHM files.
    // Native exports are rollback-journal single-file databases; admit that
    // header before any SQLite connection touches the supplied package.
    let mut header = [0u8; 20];
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source.join(PKG_DB))?
        .read_exact(&mut header)?;
    if &header[..16] != b"SQLite format 3\0" || header[18] != 1 || header[19] != 1 {
        return Err(Error::rejected(
            "portable inspection requires a single-file rollback-journal SQLite database",
        ));
    }
    validate_staged_store(&source.join(PKG_DB), &manifest)?;
    let tracker = source.join(PKG_TRACKER);
    if pm_root_of(&tracker)? != manifest.pm_root {
        return Err(Error::rejected(
            "package tracker PM root differs from manifest",
        ));
    }
    if hash_file(&source.join(PKG_MANIFEST))?.0 != manifest_sha256 {
        return Err(Error::rejected(
            "package manifest changed during inspection",
        ));
    }
    let bytes = manifest.members.iter().try_fold(0u64, |total, member| {
        total
            .checked_add(member.bytes)
            .ok_or_else(|| Error::rejected("package byte total overflows"))
    })?;
    let report = json!({
        "status": "verified_coordination_package",
        "portable": true,
        "bundle": source,
        "manifest_sha256": manifest_sha256,
        "format": manifest.format,
        "scope": manifest.scope,
        "full_workspace": manifest.full_workspace,
        "cadence": manifest.cadence,
        "inspector_revision": crate::overview::BUILD_COMMIT,
        "schema_version": manifest.schema_version,
        "tracker_revision": manifest.tracker_revision,
        "members": manifest.members.len(),
        "bytes": bytes,
        "tables": manifest.tables,
        "omitted_tables": manifest.omitted_tables,
        "omitted_rows": manifest.omitted_rows,
        "nulled_columns": manifest.nulled_columns,
        "disabled_columns": manifest.disabled_columns,
        "exclusions": manifest.exclusions,
        "redacted_message_bodies": manifest.redacted_message_bodies,
        "verification": {
            "member_paths_hashes_sizes": true,
            "exact_member_set": true,
            "database_integrity_catalog_authority": true
        },
        "source_fidelity": "not_independently_established",
        "import_ready": false
    });
    Ok(VerifiedPackage {
        manifest,
        manifest_bytes,
        manifest_sha256,
        report,
    })
}

/// Independently inspect a portable coordination package, read-only. Prints
/// integrity, schema and exclusion evidence; does not restore, start a daemon,
/// or establish import readiness.
pub fn inspect_portable(source: &Path) -> Result<Value> {
    Ok(inspect_package(source)?.report)
}

fn refuse_existing(out: &Path) -> Result<()> {
    if out.symlink_metadata().is_ok() {
        return Err(Error::rejected(format!(
            "{} already exists; export writes a new directory",
            out.display()
        )));
    }
    Ok(())
}

fn private_staging(out: &Path) -> Result<PathBuf> {
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let name = out
        .file_name()
        .ok_or_else(|| Error::rejected("export output needs a directory name"))?;
    let staging = parent.join(format!(
        ".{}.partial-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    if staging.symlink_metadata().is_ok() {
        return Err(Error::rejected(format!(
            "{} exists from an earlier attempt; remove it after inspection",
            staging.display()
        )));
    }
    fs::DirBuilder::new().mode(0o700).create(&staging)?;
    Ok(staging)
}

fn build_package(
    live: &Path,
    staging: &Path,
    tracker: &Path,
    head: &str,
    allow: &Allowlist,
    redact_history_secrets: bool,
) -> Result<PortableManifest> {
    let mut output = ArtifactDir::open(staging, false)?;
    let temp_name = format!(".{PKG_DB}.partial");
    let partial = output.file_path(temp_name.as_ref())?;
    // Private online snapshot; the live store is only read.
    artifact_snapshot(live, &mut output, temp_name.as_ref())?;
    let source_uuid = read_source_uuid(&partial)?;
    check_catalog(&partial)?;
    output.sqlite_sidecars(temp_name.as_ref())?;
    let omitted_rows = project(&partial)?;
    // Existing redaction on the private copy, then a read-only secret scan
    // and integrity check of the projected copy.
    output.sqlite_sidecars(temp_name.as_ref())?;
    let scrub = scrub(&partial, redact_history_secrets)?;
    scan_db(&partial, allow)?;
    let (integrity, schema) = inspect(&partial)?;
    require_ok(&partial, &integrity)?;
    output.clean_sidecars(temp_name.as_ref())?;
    output.publish(temp_name.as_ref(), PKG_DB.as_ref())?;
    let db = output.file_path(PKG_DB.as_ref())?;
    let tracker_dir = staging.join(PKG_TRACKER);
    copy_tracker(tracker, &tracker_dir, head)?;
    let pm_root = pm_root_of(&tracker_dir)?;
    let mut members = vec![member_for(&db, PKG_DB)?];
    members.extend(tracker_members(staging, &tracker_dir)?);
    let mut package_exclusions = exclusions();
    if redact_history_secrets {
        package_exclusions.push(
            "credential-shaped spans in UTF-8 text messages.body (opt-in; includes warn-only findings)"
                .into(),
        );
    }
    let manifest = PortableManifest {
        pm_root,
        format: PORTABLE_FORMAT.into(),
        scope: "coordination".into(),
        full_workspace: false,
        created_at: crate::issue::time::iso(now_secs() as i64),
        source_uuid,
        cadence: env!("CARGO_PKG_VERSION").into(),
        schema_version: schema,
        tracker_revision: head.into(),
        tables: COORDINATION.iter().map(|t| t.to_string()).collect(),
        omitted_tables: omitted_names().iter().map(|t| t.to_string()).collect(),
        omitted_rows,
        nulled_columns: NULLED.iter().map(|(t, c)| format!("{t}.{c}")).collect(),
        disabled_columns: DISABLED.iter().map(|(t, c)| format!("{t}.{c}")).collect(),
        exclusions: package_exclusions,
        redacted_message_bodies: scrub.redacted_message_bodies,
        members,
    };
    let text = serde_json::to_string_pretty(&manifest)?;
    write_private(&staging.join(PKG_MANIFEST), text.as_bytes())?;
    output.commit()?;
    Ok(manifest)
}

/// Every known table outside the coordination profile, in catalog order:
/// provenance, authority, then business app data. `project` deletes exactly
/// this set from the private copy and the manifest lists exactly this set.
fn omitted_names() -> Vec<&'static str> {
    CATALOG
        .iter()
        .map(|(t, _)| *t)
        .filter(|t| !COORDINATION.contains(t))
        .collect()
}

fn read_source_uuid(db: &Path) -> Result<Option<String>> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='connection_metadata'",
        [],
        |r| r.get(0),
    )?;
    if has == 0 {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT workspace_id FROM connection_metadata WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .ok())
}

/// Every table and column in the private copy must match the fixed catalog
/// exactly. Unknown tables or columns refuse; no prefix is trusted.
fn check_catalog(db: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for table in &tables {
        let Some((_, expected)) = CATALOG.iter().find(|(t, _)| t == table) else {
            return Err(Error::rejected(format!(
                "portable export refuses: table {table} is not in the known store catalog"
            )));
        };
        let cols: Vec<String> = conn
            .prepare(&format!(
                "SELECT name FROM pragma_table_info({})",
                sql_string(table)
            ))?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        // Column order is physical (ALTER appends), so compare names as an
        // exact unordered set. SQLite column names are unique per table.
        let actual: std::collections::BTreeSet<&str> = cols.iter().map(String::as_str).collect();
        let wanted: std::collections::BTreeSet<&str> = expected.iter().copied().collect();
        if cols.len() != expected.len() || actual != wanted {
            return Err(Error::rejected(format!(
                "portable export refuses: table {table} columns do not match the known catalog"
            )));
        }
    }
    check_business_guards(&conn)
}

/// Business tables whose immutability guards the export and restore rely on.
const BUSINESS_TABLES: &[&str] = &[
    "business_template_versions",
    "business_template_sources",
    "business_documents",
];

/// The business trigger set must equal the one the compiled-in schema creates.
/// Expected rows come from a scratch in-memory database built from that DDL,
/// so a stored guard is never trusted by its own SQL text. Runs before
/// `project` drops or replays any guard, and on the staged store at restore.
fn check_business_guards(conn: &Connection) -> Result<()> {
    // A store older than v38 has no business tables; there is nothing to guard.
    let present: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ({})",
            business_list()
        ),
        [],
        |r| r.get(0),
    )?;
    if present == 0 {
        return Ok(());
    }
    let trusted = Connection::open_in_memory()?;
    trusted.execute_batch(crate::store::BUSINESS_TEMPLATE_SCHEMA)?;
    let expected = business_guards(&trusted)?;
    let actual = business_guards(conn)?;
    if expected == actual {
        return Ok(());
    }
    Err(Error::rejected(
        "portable refuses: business immutable guards do not match trusted schema",
    ))
}

fn business_list() -> String {
    BUSINESS_TABLES
        .iter()
        .map(|t| sql_string(t))
        .collect::<Vec<_>>()
        .join(",")
}

/// `(name, table, sql)` of every trigger on a business table, by name.
fn business_guards(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT name,tbl_name,sql FROM sqlite_master WHERE type='trigger' AND tbl_name IN ({}) ORDER BY name",
        business_list()
    ))?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn sql_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Project the private copy to the coordination profile. Known omitted
/// tables are deleted here (the source is never touched) and their row
/// counts recorded. Returns `(table, rows_removed)`.
fn project(db: &Path) -> Result<Vec<(String, i64)>> {
    let conn = Connection::open(db)?;
    let mut omitted = Vec::new();
    let tx = conn.unchecked_transaction()?;
    // Related private rows are removed together; enforce their foreign keys at commit.
    tx.pragma_update(None, "defer_foreign_keys", true)?;
    let mut guard_sql = Vec::new();
    for name in PRIVATE_DELETE_GUARDS {
        guard_sql.push(tx.query_row(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1",
            [name],
            |r| r.get::<_, String>(0),
        )?);
        tx.execute(&format!("DROP TRIGGER {}", quote_ident(name)), [])?;
    }
    // CAD-1412: human-room transcripts reuse the coordination topic tables.
    // Scrub those rows before the room metadata is dropped, so no private
    // human post, delivery or membership leaks through the coordination
    // profile.
    if tx.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='human_rooms'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0
    {
        tx.execute(
            "DELETE FROM topic_deliveries WHERE post_id IN (SELECT p.id FROM topic_posts p JOIN topics t ON t.id=p.topic_id WHERE t.type='human_room')",
            [],
        )?;
        tx.execute(
            "DELETE FROM topic_posts WHERE topic_id IN (SELECT id FROM topics WHERE type='human_room')",
            [],
        )?;
        tx.execute(
            "DELETE FROM topic_subscriptions WHERE topic_id IN (SELECT id FROM topics WHERE type='human_room')",
            [],
        )?;
        tx.execute("DELETE FROM topics WHERE type='human_room'", [])?;
    }
    // CAD-1853 review: the four cloud-agent tables are outside the
    // coordination profile, so their connection, grant, session and receipt
    // authority never travels — but the cloud request and result TEXT reuses
    // the coordination tables (`messages`, `thread_entries`, `threads`,
    // `events`). Exporting that text against an alias whose authority rows
    // are omitted would hand whoever restores the package a cloud
    // conversation that reads as an ordinary local one. Cloud content is
    // private to the source/owner that was admitted on the live workspace,
    // so it is omitted together with the authority that admitted it. The
    // inert alias row itself stays (its `endpoint`/`params` are nulled
    // below) so an explicit `cloud-agent connect` can reuse it; until then
    // every cloud projection of that alias fails closed.
    //
    // A settled turn also routes its reply as a `worker_result` (or a
    // `worker_notice`) on the ordinary alias of whoever invoked it, and that
    // copy is what `agent_show`/`thread_read` of that alias would serve after
    // a restore. Those rows are not on the cloud alias, so they are collected
    // by their ORIGIN — `messages.origin` names the cloud message, whose id
    // is the operation id — recursively, together with the canonical rows
    // they derive from. Their thread entries, receipts (`messages.result`
    // and `error`) and events travel with the same closure; the derived row's
    // own body is never filtered by alias or source.
    let cloud = crate::cloud_agent::PROVIDER;
    let cloud_alias = "SELECT alias FROM agents WHERE provider=?1";
    let cloud_messages = format!(
        "WITH RECURSIVE cloud_message(id) AS ( \
           SELECT id FROM messages \
            WHERE alias IN ({cloud_alias}) OR source='cloud_agent' \
           UNION \
           SELECT m.id FROM messages m JOIN cloud_message c ON m.origin=c.id \
         )"
    );
    tx.execute(
        &format!(
            "{cloud_messages} DELETE FROM thread_entries WHERE \
               message_id IN (SELECT id FROM cloud_message) \
               OR json_extract(payload,'$.message') IN (SELECT id FROM cloud_message) \
               OR thread_id IN (SELECT id FROM threads WHERE alias IN ({cloud_alias}))"
        ),
        [cloud],
    )?;
    // The closure is by REFERENCE as well as by row. A completed delivery
    // duplicates its result into the alias' own `turn_finished` event
    // (`payload.message` = the delivered message id, `payload.result` = the
    // result text), so an event on an ordinary alias can carry a cloud reply
    // that the message row no longer does. Every event naming a closed-over
    // message — the routed copy, its delivery, or the cloud request itself —
    // travels with it, whichever alias holds the event. Runs BEFORE the
    // message delete: the closure is derived from `messages`.
    tx.execute(
        &format!(
            "{cloud_messages} DELETE FROM events WHERE \
               json_extract(payload,'$.message') IN (SELECT id FROM cloud_message) \
               OR json_extract(payload,'$.delivery') IN (SELECT id FROM cloud_message)"
        ),
        [cloud],
    )?;
    // Provider traffic and adapter bookkeeping are TRANSCRIPT material, not
    // coordination data, and they can embed conversation text with no
    // canonical id at all: a continuity pack quotes earlier thread content
    // verbatim, the Codex adapter forwards completed assistant messages with
    // their full text, and tool use/results and permission denials carry
    // command, query and output summaries. No id-keyed closure can find any of
    // that, and a restore resumes the board, tasks, messages and receipts — so
    // every adapter bookkeeping kind is omitted whole, together with the
    // generic provider event. This list mirrors the `cadence/<kind>` emitters
    // under `src/adapter/`: a new adapter kind that can carry content belongs
    // in it (and in the portable guard's producer shapes).
    tx.execute(
        "DELETE FROM events WHERE kind IN ( \
           'provider_event', 'fake_pack', 'claim_used', 'claude_init', \
           'claude_result', 'cloud_session', 'codex_quota', 'interrupt_ack', \
           'pane_root', 'pane_root_unrecorded', 'permission_denied', \
           'pi_extension_error', 'pi_retry', 'pi_session_reset_failed', \
           'pi_ui_request', 'ready_claimed', 'serialized_body_limited', \
           'session_compacted', 'session_minted', 'session_resume_failed', \
           'stale_result', 'tool_progress', 'tool_result', 'tool_use')",
        [],
    )?;
    // Screen text is transcript material wherever it is stored: a render
    // failure keeps the pane before and after the paste, an approval menu
    // keeps the command/subject line it displayed, and a probe's `reason` is
    // that same extracted line (Claude, Cursor and Devin fill it from the
    // screen, not from a fixed verdict). Any of those can quote an earlier
    // private reply while the event names another delivery — or no message at
    // all. The events stay, with their message, attempt, retry and probe
    // verdicts; the captured prose does not.
    tx.execute(
        "UPDATE events SET payload=json_remove(payload, \
           '$.before','$.after','$.line','$.probe.reason', \
           '$.claim_probe','$.reprobe') \
          WHERE json_valid(payload)",
        [],
    )?;
    // Two coordination events COPY that screen line into their own `reason`
    // before persisting it: a blocked delivery says which approval menu is
    // open, and a refused recovery says the same. Keep the event, the
    // message and the static verdict; drop the copied subject.
    tx.execute(
        "UPDATE events SET payload=json_remove(payload,'$.reason') \
          WHERE kind IN ('gate_wait','submit_recover_refused') \
            AND json_valid(payload)",
        [],
    )?;
    // A monitor alert materializes the scoped event payload it fired on, so the
    // same screen prose can sit one level down in `monitor_alerts.payload`.
    // The alert keeps its identity, task, kind, state and fingerprint; the
    // copied transcript does not.
    // Provider error prose has no lineage at all: a failed turn publishes the
    // provider's own message into `agents.error` and into an `attention`
    // event, and a history-refusal note is a thread entry that references its
    // delivery only in the payload. None of those can be traced back to the
    // cloud row they quoted, so the portable copy does not carry the prose:
    // the agent keeps its state, the attention marker and the note's
    // reference, without the quoted text.
    tx.execute("UPDATE agents SET error=NULL", [])?;
    // Provider transcript and runtime notes that had no running message when
    // they arrived are canonical history with NO binding: an unbound entry can
    // quote a turn whose cloud operation is omitted, and nothing ties it back.
    // A portable export carries the BOUND conversation, not unbound provider
    // or runtime transcript.
    tx.execute("DELETE FROM thread_entries WHERE message_id IS NULL", [])?;
    // A brokered permission notice copies the tool command/input summary into
    // its delivery body. The notice row, its sender and its state stay; the
    // copied prose does not.
    // A brokered notice quotes the tool command/input summary that produced
    // it, and that prose is copied into its receipt, its completion event, its
    // thread entry and the entry's own error payload. The notice row, its
    // sender, state and binding stay; every copy of the quoted text goes.
    tx.execute(
        "UPDATE messages SET body='', result=NULL, error=NULL WHERE source='worker_notice'",
        [],
    )?;
    tx.execute(
        "UPDATE thread_entries SET text='', \
           payload=json_remove(payload,'$.error') \
          WHERE message_id IN (SELECT id FROM messages WHERE source='worker_notice') \
            AND json_valid(payload)",
        [],
    )?;
    tx.execute(
        "UPDATE thread_entries SET text='' \
          WHERE message_id IN (SELECT id FROM messages WHERE source='worker_notice')",
        [],
    )?;
    tx.execute(
        "UPDATE events SET payload=json_remove(payload,'$.result') \
          WHERE kind='turn_finished' AND json_valid(payload) \
            AND json_extract(payload,'$.message') IN \
                (SELECT id FROM messages WHERE source='worker_notice')",
        [],
    )?;
    // A provider session reset keeps the provider's error text in its event.
    tx.execute(
        "UPDATE events SET payload=json_remove(payload,'$.error') \
          WHERE kind='provider_session_reset' AND json_valid(payload)",
        [],
    )?;
    tx.execute(
        "UPDATE events SET payload=json_remove(payload,'$.reason') \
          WHERE kind='attention' AND json_valid(payload)",
        [],
    )?;
    tx.execute(
        "UPDATE monitor_alerts SET payload=json_remove(payload, \
           '$.payload.before','$.payload.after','$.payload.line', \
           '$.payload.probe.reason','$.payload.claim_probe','$.payload.reprobe') \
          WHERE json_valid(payload)",
        [],
    )?;
    tx.execute(
        &format!(
            "{cloud_messages} DELETE FROM messages WHERE id IN (SELECT id FROM cloud_message)"
        ),
        [cloud],
    )?;
    tx.execute(
        &format!("DELETE FROM threads WHERE alias IN ({cloud_alias})"),
        [cloud],
    )?;
    tx.execute(
        &format!("DELETE FROM events WHERE alias IN ({cloud_alias})"),
        [cloud],
    )?;
    for table in omitted_names() {
        let present: i64 = tx.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?",
            [table],
            |r| r.get(0),
        )?;
        if present == 0 {
            // A catalog table this store predates: nothing to remove.
            omitted.push((table.to_string(), 0));
            continue;
        }
        let rows: i64 = tx.query_row(
            &format!("SELECT count(*) FROM {}", quote_ident(table)),
            [],
            |r| r.get(0),
        )?;
        omitted.push((table.to_string(), rows));
        tx.execute(&format!("DELETE FROM {}", quote_ident(table)), [])?;
    }
    for sql in &guard_sql {
        tx.execute(sql, [])?;
    }
    for (table, column) in NULLED {
        tx.execute(&format!("UPDATE {table} SET {column}=NULL"), [])?;
    }
    for (table, column) in DISABLED {
        tx.execute(&format!("UPDATE {table} SET {column}=0"), [])?;
    }
    tx.commit()?;
    conn.execute_batch("VACUUM")?;
    Ok(omitted)
}

// ---------- tracker admission and copy ----------

/// Admit an ordinary Git tracker before any git reader runs. The checks are
/// on metadata only: `.git` must be a real directory; no `objects/info/alternates`,
/// no `commondir`/`gitdir` redirection, no `worktrees`, `modules` or `info/grafts`,
/// no config `include`, no `hooks` entries beyond `.sample`, no symlink or
/// hardlink inside `.git`. The tracker must be clean. Returns HEAD.
pub fn admit_tracker(tracker: &Path) -> Result<String> {
    admit_tracker_metadata(tracker, Admission::Export)?;
    let status = git_plain(
        tracker,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !status.trim().is_empty() {
        return Err(Error::rejected(format!(
            "tracker {} refused: not clean; commit or stash before export",
            tracker.display()
        )));
    }
    git_plain(tracker, &["rev-parse", "--verify", "HEAD"])
}

/// Export refuses active hooks and `core.hooksPath`. Capture admits regular
/// installed hooks and one plain `core.hooksPath` key. The configured
/// directory is not read. Source git still forces `core.hooksPath=/dev/null`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Admission {
    Export,
    Capture,
}

fn admit_tracker_metadata(tracker: &Path, admission: Admission) -> Result<()> {
    let refuse =
        |why: &str| Error::rejected(format!("tracker {} refused: {why}", tracker.display()));
    let dotgit = tracker.join(".git");
    let meta = fs::symlink_metadata(&dotgit)
        .map_err(|_| refuse("not an ordinary checkout (.git missing)"))?;
    if !meta.is_dir() {
        return Err(refuse(
            ".git is not a plain directory (gitfile or linked worktree)",
        ));
    }
    for banned in [
        "commondir",
        "gitdir",
        "objects/info/alternates",
        "info/grafts",
        "modules",
        "worktrees",
        "shallow",
    ] {
        if fs::symlink_metadata(dotgit.join(banned)).is_ok() {
            return Err(refuse(&format!("{banned} is present; unsupported layout")));
        }
    }
    // Export only. `is_dir` follows links, and the byte compare reads hook
    // files. Capture must not do either; the metadata walk still refuses a
    // symlink, hardlink, or special file, including inside `hooks/`.
    if admission == Admission::Export {
        let hooks = dotgit.join("hooks");
        if hooks.is_dir() && has_active_hooks(&hooks)? {
            return Err(refuse("active git hooks"));
        }
    }
    check_metadata_tree(&dotgit, tracker)?;
    check_config_keys(&dotgit.join("config"), tracker, admission)?;
    Ok(())
}

/// Read `.git/config` as keys only, before git loads it. Includes and
/// conditional includes refuse, as do keys that name a path or command git
/// would run or read elsewhere. No values enter any evidence.
///
/// Capture ignores a plain `[core]` `hooksPath` key and does not parse its
/// value. A subsection, or any other path or command key, still refuses.
fn check_config_keys(config: &Path, tracker: &Path, admission: Admission) -> Result<()> {
    let refuse =
        |why: String| Error::rejected(format!("tracker {} refused: {why}", tracker.display()));
    let text = match fs::read(config) {
        Ok(b) => b,
        Err(_) => return Err(refuse("unreadable .git/config".into())),
    };
    let text = String::from_utf8(text).map_err(|_| refuse("non-UTF-8 .git/config".into()))?;
    let mut section = String::new();
    let mut plain_section = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(head) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let mut parts = head.split_whitespace();
            let name = parts.next().unwrap_or_default().to_ascii_lowercase();
            if name == "include" || name == "includeif" {
                return Err(refuse(format!("config {name} section is not supported")));
            }
            section = name;
            plain_section = parts.next().is_none();
            continue;
        }
        let key = line
            .split(['=', ' ', '\t'])
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let full = format!("{section}.{key}");
        if admission == Admission::Capture && plain_section && full == "core.hookspath" {
            continue;
        }
        const PATH_OR_COMMAND: &[&str] = &[
            "core.worktree",
            "core.hookspath",
            "core.attributesfile",
            "core.fsmonitor",
            "core.gitproxy",
            "core.sshcommand",
            "core.askpass",
            "core.pager",
            "core.editor",
            "core.alternaterefscommand",
            "core.alternaterefsprefixes",
            "core.excludesfile",
            "core.commongitdir",
        ];
        if PATH_OR_COMMAND.contains(&full.as_str()) {
            return Err(refuse(format!(
                "config key {full} names a path or command; not supported"
            )));
        }
        if key.starts_with("include") {
            return Err(refuse(format!("config key {full} is an include directive")));
        }
    }
    Ok(())
}

/// Walk `.git` metadata without following links. Every entry must be a
/// regular file or directory owned by the current user; a symlink, device,
/// hardlinked file, or foreign owner refuses.
fn check_metadata_tree(dir: &Path, tracker: &Path) -> Result<()> {
    let me = unsafe { libc::geteuid() };
    let mut count = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d)? {
            let entry = entry?;
            count += 1;
            if count > 10_000 {
                return Err(Error::rejected(format!(
                    "tracker {} refused: metadata inventory exceeds 10000 entries",
                    tracker.display()
                )));
            }
            let path = entry.path();
            let meta = path.symlink_metadata()?;
            if meta.file_type().is_symlink() {
                return Err(Error::rejected(format!(
                    "tracker {} refused: symlink in .git metadata",
                    tracker.display()
                )));
            }
            if meta.uid() != me {
                return Err(Error::rejected(format!(
                    "tracker {} refused: foreign-owned .git metadata",
                    tracker.display()
                )));
            }
            if meta.is_dir() {
                stack.push(path);
            } else if !meta.is_file() || meta.nlink() > 1 {
                return Err(Error::rejected(format!(
                    "tracker {} refused: special or hardlinked .git entry",
                    tracker.display()
                )));
            }
        }
    }
    Ok(())
}

/// Active hooks refuse, except the exact native-managed hooks that
/// `issue init` installs (same name, same bytes, regular single-link file).
/// Those never run here: every git call disables hooks.
fn has_active_hooks(dir: &Path) -> Result<bool> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".sample") {
            continue;
        }
        let path = entry.path();
        let meta = path.symlink_metadata()?;
        if !meta.is_file() || meta.nlink() > 1 {
            return Ok(true);
        }
        let native = crate::issue::hooks::HOOKS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, bytes)| bytes.as_bytes());
        match native {
            Some(bytes) if fs::read(&path)? == bytes => {}
            _ => return Ok(true),
        }
    }
    Ok(false)
}

/// The PM root in a copied tracker: `.` when `pm.yaml` sits at the root,
/// `pm` when it sits in a `pm/` dir, `None` when neither. Both is ambiguous.
fn pm_root_of(tracker_dir: &Path) -> Result<Option<String>> {
    match (
        tracker_dir.join("pm.yaml").is_file(),
        tracker_dir.join("pm").join("pm.yaml").is_file(),
    ) {
        (true, true) => Err(Error::rejected(
            "tracker has pm.yaml at its root and in pm/; the PM root is ambiguous",
        )),
        (true, false) => Ok(Some(".".into())),
        (false, true) => Ok(Some("pm".into())),
        (false, false) => Ok(None),
    }
}

/// Run git against a throwaway index copy, with no user, system or global
/// config, no hooks and no fsmonitor. The tracker's own index is never
/// refreshed or written.
fn git_plain(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git_bytes(dir, args)?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Run git and return raw stdout. No UTF-8 conversion and no trimming, so
/// a blob's bytes (including trailing newlines and non-UTF-8 content) are
/// exported exactly as committed.
///
/// `-c core.hooksPath=/dev/null` is installed before `args`, with a cleared
/// environment and a private index. Capture admits a source `core.hooksPath`
/// key only because every source git command comes through here. A later
/// `-c core.hooksPath` in `args` would replace the override.
fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let scratch = std::env::temp_dir().join(format!("cad1163-git-{}", nonce()));
    fs::create_dir_all(&scratch)?;
    let index = scratch.join("index");
    let real_index = dir.join(".git").join("index");
    if real_index.is_file() {
        fs::copy(&real_index, &index)?;
    }
    let mut cmd = Command::new("git");
    cmd.arg("--git-dir")
        .arg(dir.join(".git"))
        .arg("--work-tree")
        .arg(dir)
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.splitIndex=false",
        ])
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("GIT_INDEX_FILE", &index)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("HOME", &scratch);
    let out = crate::reaper::output(&mut cmd)
        .map_err(|e| Error::rejected(format!("git unavailable: {e}")));
    let _ = fs::remove_dir_all(&scratch);
    let out = out?;
    if !out.status.success() {
        return Err(Error::rejected(format!("git {} failed", args.join(" "))));
    }
    Ok(out.stdout)
}

/// Credential-bearing tracker files: refused by name before any blob is read,
/// on export and again on restore. A tracker is never copied as a credential store.
fn check_tracker_path(path: &str) -> Result<()> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let secret_name = name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.ends_with(".p12")
        || name.ends_with(".pfx")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.starts_with("id_ecdsa")
        || name.starts_with("id_dsa")
        || matches!(
            name.as_str(),
            ".netrc" | ".npmrc" | ".pypirc" | ".git-credentials" | "credentials" | "auth.json"
        );
    if secret_name {
        return Err(Error::rejected(format!(
            "tracker file {path} is credential-named; the portable tracker refuses it"
        )));
    }
    Ok(())
}

/// Closed decoder for a portable `pm.yaml`. Tracker metadata mirrors the
/// native `PmConfig` tree; runtime sections reuse their closed native types.
/// Unknown fields refuse at every level. Values are never copied into an
/// error: a mismatch reports only the generic reason.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // metadata fields close the schema; runtime strings are scanned
struct PortablePmConfig {
    schema: u32,
    #[serde(default)]
    statuses: Vec<String>,
    #[serde(default)]
    link_types: Vec<String>,
    #[serde(default)]
    artifact_max_bytes: u64,
    #[serde(default)]
    notes_dir: String,
    #[serde(default)]
    wiki: PortableWiki,
    #[serde(default)]
    review: PortableReview,
    #[serde(default)]
    approvals: PortableApprovals,
    #[serde(default)]
    pi: crate::pi_policy::PiPolicy,
    #[serde(default)]
    host: crate::doctor::host::HostOverrides,
    #[serde(default)]
    hosted: crate::lease::Hosted,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // validation-only: the fields close the schema, they are not read
struct PortableWiki {
    #[serde(default)]
    max_upload_bytes: u64,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // validation-only: the fields close the schema, they are not read
struct PortableReview {
    #[serde(default)]
    pair: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    never: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // validation-only: the fields close the schema, they are not read
struct PortableApprovals {
    #[serde(default)]
    tailnet_logins: Vec<String>,
}

/// Top-level `pm.yaml` keys (the closed `PortablePmConfig` field set). Only a
/// key name is reported, never a value.
const PM_CONFIG_KEYS: &[&str] = &[
    "schema",
    "statuses",
    "link_types",
    "artifact_max_bytes",
    "notes_dir",
    "wiki",
    "review",
    "approvals",
    "pi",
    "host",
    "hosted",
];

fn check_tracker_content(path: &str, bytes: &[u8]) -> Result<()> {
    if path != "pm/pm.yaml" && path != "pm.yaml" {
        return Ok(());
    }
    let refuse = |why: &str| Error::rejected(format!("tracker file {path} refused: {why}"));
    let text = std::str::from_utf8(bytes).map_err(|_| refuse("not valid YAML"))?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|_| refuse("not valid YAML"))?;
    let map = value.as_mapping().ok_or_else(|| refuse("not a mapping"))?;
    for (key, section) in map {
        let name = key.as_str().ok_or_else(|| refuse("non-string key"))?;
        if !PM_CONFIG_KEYS.contains(&name) {
            return Err(refuse(&format!(
                "unknown top-level key {name}; credential-bearing config is not portable"
            )));
        }
        if matches!(name, "pi" | "host" | "hosted") && !section.is_mapping() {
            return Err(refuse(&format!("runtime section {name} must be a mapping")));
        }
    }
    let config = serde_yaml::from_value::<PortablePmConfig>(value).map_err(|_| {
        refuse("does not match the pm.yaml schema (unknown or mistyped key at some level)")
    })?;
    let check_text = |text: &str| -> Result<()> {
        if crate::secret::scan(text, Some(path))?
            .iter()
            .any(|finding| finding.severity == crate::secret::Severity::Block)
        {
            return Err(refuse("credential-shaped config content is not portable"));
        }
        Ok(())
    };
    // Raw text covers comments; decoded runtime strings cover YAML escapes.
    check_text(text)?;
    for value in config
        .pi
        .providers
        .iter()
        .chain(&config.pi.agentic_providers)
        .chain(&config.pi.models.allow)
        .chain(config.pi.models.master_allow.iter().flatten())
        .chain(config.pi.models.worker_allow.iter().flatten())
        .chain(config.pi.models.default.master.iter())
        .chain(config.pi.models.default.worker.iter())
        .chain(config.host.priority_lanes.iter().flatten())
        .chain(
            config
                .host
                .auto_stop_idle_secs_by_provider
                .iter()
                .flat_map(|providers| providers.keys()),
        )
        .chain(config.hosted.lease.iter())
    {
        check_text(value)?;
    }
    Ok(())
}

/// Copy the tracked regular files at HEAD (no `.git` directory is copied).
fn copy_tracker(tracker: &Path, dst: &Path, head: &str) -> Result<()> {
    fs::DirBuilder::new().mode(0o700).create(dst)?;
    let listing = git_plain(tracker, &["ls-tree", "-r", "-z", "--full-tree", head])?;
    for record in listing.split('\0').filter(|r| !r.is_empty()) {
        let (meta, path) = record
            .split_once('\t')
            .ok_or_else(|| Error::rejected("malformed tracker listing"))?;
        let mode = meta.split(' ').next().unwrap_or_default();
        if mode != "100644" && mode != "100755" {
            return Err(Error::rejected(format!(
                "tracker entry {path} has mode {mode}; only regular files are exported"
            )));
        }
        let rel = Path::new(path);
        if !safe_relative(rel) {
            return Err(Error::rejected(format!("unsafe tracker path {path}")));
        }
        check_tracker_path(path)?;
        let blob = git_bytes(tracker, &["show", &format!("{head}:{path}")])?;
        check_tracker_content(path, &blob)?;
        let target = dst.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        write_private_mode(&target, &blob, if mode == "100755" { 0o700 } else { 0o600 })?;
    }
    Ok(())
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn tracker_members(root: &Path, dir: &Path) -> Result<Vec<Member>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d)? {
            let path = entry?.path();
            let meta = path.symlink_metadata()?;
            if meta.file_type().is_symlink() {
                return Err(Error::rejected("tracker copy contains a symlink"));
            }
            if meta.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                out.push(member_for(&path, &rel)?);
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn member_for(file: &Path, rel: &str) -> Result<Member> {
    let (sha256, bytes) = hash_file(file)?;
    Ok(Member {
        path: rel.to_string(),
        bytes,
        sha256,
    })
}

fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    write_private_mode(path, bytes, 0o600)
}

fn write_private_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(bytes)?;
    f.set_permissions(fs::Permissions::from_mode(mode))?;
    Ok(())
}

// ---------- publish ----------

/// Publish the staged package as `out` with a no-replace rename. A
/// concurrently created `out` makes the rename fail rather than replace it.
fn publish(staging: &Path, out: &Path, manifest: &PortableManifest) -> Result<Value> {
    if let Err(e) = rename_noreplace(staging, out) {
        let _ = fs::remove_dir_all(staging);
        return Err(e);
    }
    Ok(json!({
        "portable": true,
        "bundle": out,
        "format": manifest.format,
        "scope": manifest.scope,
        "full_workspace": manifest.full_workspace,
        "tracker_revision": manifest.tracker_revision,
        "schema_version": manifest.schema_version,
        "members": manifest.members.len(),
        "tables": manifest.tables,
        "omitted_tables": manifest.omitted_tables,
        "omitted_rows": manifest.omitted_rows,
        "exclusions": manifest.exclusions,
        "redacted_message_bodies": manifest.redacted_message_bodies,
    }))
}

/// Refusal used on platforms without a no-replace rename. Nothing is published.
#[cfg(not(target_os = "linux"))]
pub(super) const NO_ATOMIC_PUBLISH: &str =
    "atomic no-replace publication is only implemented on Linux; nothing was published";

/// `renameat2(RENAME_NOREPLACE)`: fails with EEXIST when `dst` exists.
#[cfg(target_os = "linux")]
fn rename_noreplace(src: &Path, dst: &Path) -> Result<()> {
    let s =
        CString::new(src.as_os_str().as_bytes()).map_err(|_| Error::rejected("path has NUL"))?;
    let d =
        CString::new(dst.as_os_str().as_bytes()).map_err(|_| Error::rejected("path has NUL"))?;
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            s.as_ptr(),
            libc::AT_FDCWD,
            d.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(if err.raw_os_error() == Some(libc::EEXIST) {
            Error::rejected(format!(
                "{} already exists; export will not replace it",
                dst.display()
            ))
        } else {
            Error::rejected(format!("cannot publish {}: {err}", dst.display()))
        });
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn rename_noreplace(_src: &Path, _dst: &Path) -> Result<()> {
    Err(Error::rejected(NO_ATOMIC_PUBLISH))
}

fn exclusions() -> Vec<String> {
    [
        "authority: platform_credentials, platform_grants, platform_defaults, platform_drafts, platform_effects, app_grants, rollout_grants, rollout_leases, staging_grants, staging_instances, daemon_build, assistants, assist_requests, assist_stall_checks, assist_job_shares, job_subscribers (rows deleted from the private copy)",
        "live execution authority nulled: agents.generation, pid, pid_start, endpoint, thread_id, session_id, params; messages.turn_id; token-shaped values redacted by the existing scrub",
        "imported actors disabled: agents.enabled=0; monitors dispatch flags=0",
        "provenance purged: connection_metadata (source UUID recorded in the manifest only), model_defaults, settings_authority",
        "business app data outside the coordination profile: app_*, crm_*, social_publish_intents, cloud_dispatch_outbox, chat_files, artifacts (rows deleted from the private copy; declared omitted, not a full workspace)",
        "cloud-agent private content: cloud_agents, cloud_agent_sources, cloud_agent_sessions and cloud_agent_operations, plus the cloud alias' messages, thread entries, threads and events and every request/result copy DERIVED from a cloud message by origin (a routed worker_result/worker_notice on an ordinary alias, with its thread entries and receipt) (rows deleted from the private copy). The alias row itself is kept inert with no authority; cloud projections of it fail closed until it is reconnected explicitly",
        "local checkouts, process identity, sockets and runtime files: not included",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ---------- inactive restore ----------

/// Name of the commit journal. Its presence means a restore is mid-publish
/// and the destination must not start until recovered by the operator.
const JOURNAL: &str = "portable-restore.journal";

/// Restore a portable package into `destination`, an explicit state dir.
///
/// Transaction, in order:
/// 1. Verify the package members and manifest (bytes, digests, paths, types).
/// 2. Take the destination's `cadence.lock` (the same `flock` a daemon holds)
///    and refuse a destination that is not empty or has leftovers.
/// 3. Stage the core store and tracker in a fresh private directory whose
///    name carries a random nonce, never a fixed name a caller could own.
/// 4. Validate the staged store (catalog, exact columns, authority empty,
///    agents disabled, live fields null, FK check, schema, integrity) and
///    re-hash every staged member.
/// 5. Write the journal, then rename the staged set into place, the marker
///    last inside the staged set so `cadence.sqlite3` never appears without
///    it. The marker is what stops a daemon from starting over inactive data.
///
/// Any failure before step 5 leaves the destination byte-identical.
pub fn restore_inactive(
    source: &Path,
    destination: &Path,
    opts: &PortableRestoreOptions,
) -> Result<Value> {
    if opts.replace {
        return Err(Error::rejected(
            "replacing a destination needs native operator admission, which is not wired yet; nothing was written",
        ));
    }
    let manifest = verify_package(source)?;
    refuse_nonempty(destination)?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(Error::rejected(
            "destination parent directory does not exist",
        ));
    }
    // No parent lock: the parent is not the daemon's state dir, and a flock
    // there would only create an unrelated `cadence.lock`. The destination
    // is new-only, so the no-replace rename at publish is the race guard: a
    // concurrent create makes it fail and nothing is merged.
    refuse_nonempty(destination)?;
    // The whole inactive set is staged beside the destination and published
    // with one no-replace rename: DB, tracker and marker appear together or
    // not at all.
    let stage = parent.join(format!(".portable-stage-{}", nonce()));
    fs::DirBuilder::new().mode(0o700).create(&stage)?;
    if let Err(error) = stage_and_validate(source, &stage, &manifest) {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    publish_stage(&stage, destination)?;
    Ok(json!({
        "restored": true,
        "state": "committed_inactive",
        "destination": destination,
        "pm_dir": manifest.pm_root.as_deref().map(|rel| destination.join(TRACKER_DIR).join(rel)),
        "tracker_revision": manifest.tracker_revision,
        "schema_version": manifest.schema_version,
        "members": manifest.members.len(),
    }))
}

/// AOS-339/CAD-1705: restore a package into a NEW directory and activate it,
/// manual mode with execution disabled, in one step. The destination must not
/// exist: this path can never activate, overwrite or touch an existing managed
/// root or a live state dir, so the terminal-anchored operator gate on
/// `portable_activate` keeps applying to every root that already exists. The
/// activation runs on the private stage before the no-replace publish, so a
/// refusal leaves nothing behind.
pub fn restore_activate_disabled(
    source: &Path,
    destination: &Path,
    reconciliation: &[crate::backup::portable_activate::Reconciliation],
) -> Result<Value> {
    let manifest = verify_package(source)?;
    let digest = crate::backup::portable_replace::package_digest(source)?;
    refuse_existing_activation(destination)?;
    // The package and the target must be disjoint: staging beside, inside or
    // over the package would read and write the same tree.
    let target = crate::backup::portable_replace::offline_activation_target(source, destination)?;
    let parent = target
        .parent()
        .ok_or_else(|| Error::rejected("destination has no parent directory"))?;
    let stage = parent.join(format!(".portable-stage-{}", nonce()));
    fs::DirBuilder::new().mode(0o700).create(&stage)?;
    let outcome = (|| {
        stage_and_validate(source, &stage, &manifest)?;
        let before = crate::backup::portable_replace::snapshot_target_dir(&stage)?;
        let mut report = crate::backup::portable_activate::activate_in_place(
            &stage,
            &before,
            &digest,
            reconciliation,
        )?;
        publish_stage(&stage, &target)?;
        if let Some(obj) = report.as_object_mut() {
            obj.insert("restored".into(), json!(true));
            obj.insert("destination".into(), json!(target));
            obj.insert("package_digest".into(), json!(digest));
        }
        Ok(report)
    })();
    if outcome.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    outcome
}

/// AOS-339/CAD-1705: the read-only activation plan for a package. Reads the
/// package's own store and writes nothing persistent.
pub fn activate_preflight(source: &Path) -> Result<Value> {
    verify_package(source)?;
    let digest = crate::backup::portable_replace::package_digest(source)?;
    let required = crate::backup::portable_activate::required_inventory(source)?;
    Ok(json!({
        "package_digest": digest,
        "execution_mode": "disabled",
        "bindings": [],
        "required_reconciliation": required.iter().map(|r| json!({
            "kind": r.kind, "id": r.id, "status": r.status, "action": "retain_inert"
        })).collect::<Vec<_>>(),
    }))
}

/// A destination that exists at all is refused here, unlike `restore_inactive`,
/// which accepts an empty directory.
fn refuse_existing_activation(destination: &Path) -> Result<()> {
    match destination.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::rejected(format!(
            "cannot inspect {}: {error}",
            destination.display()
        ))),
        Ok(_) => Err(Error::rejected(
            "destination exists; an offline activation restores into a new directory only",
        )),
    }
}

fn nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Copy the core and tracker into `stage`, verify the copies, write the
/// inactive marker, and re-hash every staged member before commit.
fn stage_and_validate(source: &Path, stage: &Path, manifest: &PortableManifest) -> Result<()> {
    stage_members_and_validate(source, stage, manifest)?;
    // A managed root carries the daemon's zero-byte lock inode, created
    // here by the restore that publishes it (replacement never creates it).
    crate::backup::portable_replace::create_stage_lock(stage)?;
    write_private(
        &stage.join(INACTIVE_MARKER),
        serde_json::to_string_pretty(&restore_marker(manifest))?.as_bytes(),
    )?;
    verify_staged_members(stage, manifest)
}

/// The inactive marker for a fresh restore. Replacement writes its own
/// marker with a fresh generation.
fn restore_marker(manifest: &PortableManifest) -> Value {
    json!({
        "state": "committed_inactive",
        "format": manifest.format,
        "tracker_revision": manifest.tracker_revision,
        "source_uuid": manifest.source_uuid,
        "schema_version": manifest.schema_version,
        "members": manifest.members.len(),
        "activation": "not performed; native operator admission required",
    })
}

/// Copy and validate the core store and tracker into `stage`. No marker.
fn stage_members_and_validate(
    source: &Path,
    stage: &Path,
    manifest: &PortableManifest,
) -> Result<()> {
    let db = stage.join(PKG_DB);
    copy_private_file(&source.join(PKG_DB), &db)?;
    copy_tree(&source.join(PKG_TRACKER), &stage.join(TRACKER_DIR))?;
    validate_staged_store(&db, manifest)?;
    verify_tracker_content(&stage.join(TRACKER_DIR), manifest)?;
    if let Some(rel) = manifest.pm_root.as_deref() {
        crate::issue::Pm::at(&stage.join(TRACKER_DIR).join(rel))?;
    }
    Ok(())
}

/// Re-hash every staged core and tracker member immediately before commit:
/// a source change after verify but before copy must not survive.
fn verify_staged_members(stage: &Path, manifest: &PortableManifest) -> Result<()> {
    for m in manifest.members.iter() {
        let staged = match m.path.strip_prefix(&format!("{PKG_TRACKER}/")) {
            Some(rel) => stage.join(TRACKER_DIR).join(rel),
            None => stage.join(&m.path),
        };
        let (sha, bytes) = hash_file(&staged)?;
        if sha != m.sha256 || bytes != m.bytes {
            return Err(Error::rejected(format!(
                "staged member {} does not match the manifest",
                m.path
            )));
        }
    }
    Ok(())
}

/// Publish the verified stage as the destination in one no-replace rename.
/// A destination that appears concurrently makes the rename fail, and the
/// stage is removed; nothing is ever merged into or overwritten inside an
/// existing destination.
fn publish_stage(stage: &Path, destination: &Path) -> Result<()> {
    if let Err(error) = rename_noreplace(stage, destination) {
        let _ = fs::remove_dir_all(stage);
        return Err(error);
    }
    if let Some(parent) = destination.parent() {
        sync_dir(parent);
    }
    Ok(())
}

/// Read the manifest and verify every listed member: plain relative paths,
/// the exact member set (core store plus tracker files, nothing else),
/// exact bytes and sha256, and only regular non-linked files.
fn verify_package(source: &Path) -> Result<PortableManifest> {
    let bytes = fs::read(source.join(PKG_MANIFEST))
        .map_err(|e| Error::rejected(format!("cannot read manifest: {e}")))?;
    verify_manifest_bytes(source, &bytes)
}

/// The manifest bytes parsed and every member check applied.
/// Verification works on the caller-supplied exact bytes, so the digest a
/// caller binds is provably the digest of what was checked.
fn verify_manifest_bytes(source: &Path, manifest_bytes: &[u8]) -> Result<PortableManifest> {
    let manifest: PortableManifest = serde_json::from_slice(manifest_bytes)
        .map_err(|e| Error::rejected(format!("malformed manifest: {e}")))?;
    if manifest.format != PORTABLE_FORMAT
        || manifest.scope != "coordination"
        || manifest.full_workspace
    {
        return Err(Error::rejected(
            "manifest is not a coordination portable package",
        ));
    }
    if !matches!(manifest.pm_root.as_deref(), None | Some(".") | Some("pm")) {
        return Err(Error::rejected(
            "manifest pm_root is not an admitted PM root",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for m in &manifest.members {
        let rel = Path::new(&m.path);
        if !safe_relative(rel) {
            return Err(Error::rejected(format!("unsafe member path {}", m.path)));
        }
        if m.path != PKG_DB && !m.path.starts_with(&format!("{PKG_TRACKER}/")) {
            return Err(Error::rejected(format!(
                "member {} is outside the core store and tracker",
                m.path
            )));
        }
        if !seen.insert(m.path.clone()) {
            return Err(Error::rejected(format!("duplicate member {}", m.path)));
        }
        if let Some(tracked) = m.path.strip_prefix(&format!("{PKG_TRACKER}/")) {
            check_tracker_path(tracked)?;
            let file = source.join(rel);
            require_regular(&file)?;
            check_tracker_content(tracked, &fs::read(&file)?)?;
        }
        let file = source.join(rel);
        require_regular(&file)?;
        let (sha, bytes) = hash_file(&file)
            .map_err(|_| Error::rejected(format!("member {} missing or unreadable", m.path)))?;
        if sha != m.sha256 || bytes != m.bytes {
            return Err(Error::rejected(format!(
                "member {} changed (digest or size mismatch)",
                m.path
            )));
        }
    }
    if !seen.contains(PKG_DB) {
        return Err(Error::rejected("package has no store member"));
    }
    let mut actual = Vec::new();
    collect_package_members(source, source, &mut actual)?;
    for p in &actual {
        if p != PKG_MANIFEST && !seen.contains(p) {
            return Err(Error::rejected(format!(
                "package contains unlisted member {p}"
            )));
        }
    }
    Ok(manifest)
}

/// A member must be a regular file with a single link: not a symlink,
/// device, fifo, socket or hardlink.
fn require_regular(path: &Path) -> Result<()> {
    let meta = path
        .symlink_metadata()
        .map_err(|_| Error::rejected(format!("{} is missing", path.display())))?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(Error::rejected(format!(
            "{} is not a plain single-link regular file",
            path.display()
        )));
    }
    Ok(())
}

fn collect_package_members(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = path.symlink_metadata()?;
        if meta.file_type().is_symlink() {
            return Err(Error::rejected("package contains a symlink"));
        }
        if meta.is_dir() {
            collect_package_members(root, &path, out)?;
        } else {
            out.push(
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    Ok(())
}

/// A destination that exists must be an empty directory. Checked before any
/// create or lock, so a refused restore leaves a nonempty destination untouched.
fn refuse_nonempty(destination: &Path) -> Result<()> {
    match destination.symlink_metadata() {
        Err(_) => Ok(()),
        Ok(meta) if !meta.is_dir() => {
            Err(Error::rejected("destination exists and is not a directory"))
        }
        Ok(_) => {
            let any = fs::read_dir(destination)?.next().is_some();
            if any {
                return Err(Error::rejected(
                    "destination is not empty; replacement needs native operator admission",
                ));
            }
            Ok(())
        }
    }
}

/// Validate the staged store on the receiver: integrity, FK, schema, the
/// exact closed catalog, and the cell policy. Source identity is read only
/// from the manifest, never from the staged store.
fn validate_staged_store(db: &Path, manifest: &PortableManifest) -> Result<()> {
    let (integrity, schema) = inspect(db)?;
    require_ok(db, &integrity)?;
    if schema != manifest.schema_version {
        return Err(Error::rejected(
            "staged store schema does not match the manifest",
        ));
    }
    if schema > crate::rollout::SCHEMA_VERSION {
        return Err(Error::rejected("staged store is newer than this binary"));
    }
    check_catalog(db)?;
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let fk = conn
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |_| Ok(()))?
        .count();
    if fk > 0 {
        return Err(Error::rejected(format!(
            "staged store has {fk} foreign-key violation(s)"
        )));
    }
    for table in omitted_names() {
        let present: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?",
            [table],
            |r| r.get(0),
        )?;
        if present == 0 {
            continue;
        }
        let rows: i64 = conn.query_row(
            &format!("SELECT count(*) FROM {}", quote_ident(table)),
            [],
            |r| r.get(0),
        )?;
        if rows > 0 {
            return Err(Error::rejected(format!(
                "staged store carries omitted table {table} ({rows} rows)"
            )));
        }
    }
    let enabled: i64 = conn.query_row("SELECT count(*) FROM agents WHERE enabled<>0", [], |r| {
        r.get(0)
    })?;
    if enabled > 0 {
        return Err(Error::rejected("staged store enables imported actors"));
    }
    for (table, column) in NULLED {
        let live: i64 = conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {column} IS NOT NULL"),
            [],
            |r| r.get(0),
        )?;
        if live > 0 {
            return Err(Error::rejected(format!(
                "staged store carries live authority {table}.{column}"
            )));
        }
    }
    for (table, column) in DISABLED {
        let on: i64 = conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {column}<>0"),
            [],
            |r| r.get(0),
        )?;
        if on > 0 {
            return Err(Error::rejected(format!(
                "staged store enables {table}.{column}"
            )));
        }
    }
    Ok(())
}

/// The tracker copy must match the manifest's tracker members: same set,
/// same bytes, same digests, and only regular files.
fn verify_tracker_content(tracker: &Path, manifest: &PortableManifest) -> Result<()> {
    // Actual members are relative to the tracker dir; manifest members carry
    // the package's `tracker/` prefix. Compare on the path below that prefix.
    let prefix = format!("{PKG_TRACKER}/");
    let actual = tracker_members(tracker.parent().unwrap_or(tracker), tracker)?;
    let expected: Vec<&Member> = manifest
        .members
        .iter()
        .filter(|m| m.path.starts_with(&prefix))
        .collect();
    if actual.len() != expected.len() {
        return Err(Error::rejected(
            "tracker copy member set differs from the manifest",
        ));
    }
    for m in expected {
        let rel = &m.path[prefix.len()..];
        let want = format!("{TRACKER_DIR}/{rel}");
        match actual.iter().find(|a| a.path == want) {
            Some(a) if a.sha256 == m.sha256 && a.bytes == m.bytes => {}
            _ => {
                return Err(Error::rejected(format!(
                    "tracker member {} does not match the manifest",
                    m.path
                )))
            }
        }
    }
    Ok(())
}

fn copy_private_file(src: &Path, dst: &Path) -> Result<()> {
    require_regular(src)?;
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(src)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dst)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::DirBuilder::new().mode(0o700).create(dst)?;
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((s, d)) = stack.pop() {
        for entry in fs::read_dir(&s)? {
            let entry = entry?;
            let from = entry.path();
            let to = d.join(entry.file_name());
            let meta = from.symlink_metadata()?;
            if meta.is_dir() {
                fs::DirBuilder::new().mode(0o700).create(&to)?;
                stack.push((from, to));
            } else {
                copy_private_file(&from, &to)?;
            }
        }
    }
    Ok(())
}

/// True when the marker is an `activated` marker (its receipt is checked by
/// the fence loader, not here).
fn activated_root(state_dir: &Path) -> bool {
    fs::read_to_string(state_dir.join(INACTIVE_MARKER))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| v["state"].as_str() == Some(crate::backup::portable_activate::ACTIVATED))
}

/// Refuse daemon start while a portable restore is inactive or mid-publish.
/// Any journal, any marker and any unreadable or malformed marker refuses,
/// so a damaged marker or a crash mid-publish cannot let a daemon start.
pub fn refuse_if_inactive(state_dir: &Path) -> Result<()> {
    // An activated, execution-disabled root is admitted here only; the
    // fence loader (`portable_fence::load`) then requires its matching
    // receipt and refuses any mismatch. An inactive marker still refuses.
    if activated_root(state_dir) {
        return Ok(());
    }
    if state_dir.join(JOURNAL).symlink_metadata().is_ok() {
        return Err(Error::rejected(format!(
            "{} holds a portable restore journal (a publish did not finish); inspect and recover before starting the daemon",
            state_dir.display()
        )));
    }
    let marker = state_dir.join(INACTIVE_MARKER);
    match fs::symlink_metadata(&marker) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::rejected(format!(
            "cannot read {}: {e}; refusing to start",
            marker.display()
        ))),
        Ok(_) => {
            let parsed = fs::read_to_string(&marker)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok());
            let state = parsed
                .as_ref()
                .and_then(|v| v["state"].as_str())
                .unwrap_or("malformed");
            Err(Error::rejected(format!(
                "{} holds a portable restore ({state}); activate it through the native operator path before starting the daemon",
                state_dir.display()
            )))
        }
    }
}

pub fn activate(_state_dir: &Path, _proof: &NativeOperatorProof) -> Result<Value> {
    Err(Error::rejected(
        "activation requires native operator admission, which is not wired yet; the restored data stays inactive",
    ))
}

/// Closed verifier for replacement: the same package verification as
/// [`restore_inactive`], exposed crate-internally.
pub(crate) fn verify_package_for_replace(source: &Path) -> Result<PortableManifest> {
    verify_package(source)
}

/// Stage and validate the complete inactive member set into the empty
/// private `stage` dir. Writes no marker: replacement writes its own.
pub(crate) fn stage_inactive_members(
    source: &Path,
    stage: &Path,
    manifest: &PortableManifest,
) -> Result<()> {
    stage_members_and_validate(source, stage, manifest)?;
    verify_staged_members(stage, manifest)
}
