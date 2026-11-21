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
//!
//! CAD-2215: a coordination package never carries private or private-capable
//! state. Member-private job ownership, queued member requests, member
//! assistants and subscriptions, app contexts, app team defaults,
//! per-installation app record files, app conversations, app member grants
//! (revoked tombstones included), chat attachments, issue artifacts,
//! human-room transcripts and membership, cloud-agent state, the private
//! delivery and publication ledger, publication comments and a private
//! project policy are all outside the profile, so rather than dropping them
//! the export classifies the source first: any such record — or one it cannot
//! read — refuses with no package written, and the source keeps its data and
//! receipts. Only a workspace proven to hold none of them exports.
//! The store snapshot, the pinned tracker tree and the private ledger are
//! separate sources, not one atomic snapshot, so the source is classified
//! again after the package is built: private work observed during packaging
//! refuses publication instead of being published outside the classification
//! that admitted it.
//! Inspection and restore apply the same rule to a package: the PM root is
//! derived from the verified member inventory (never only from the manifest
//! field), a project-shaped record that root cannot authorize refuses, and a
//! staged store carrying an app conversation, a human-room topic or a
//! cloud-agent marker, or a tracker carrying a publication comment or a
//! private project policy, is refused rather than imported without the
//! provenance it would lose. Every tracker member is read once and that same
//! buffer is hashed and classified for the package, on inspection and on the
//! staged restore before publication.

#![deny(clippy::undocumented_unsafe_blocks)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{artifact_snapshot, inspect, quote_ident, require_ok, scan_db, scrub, ArtifactDir};
use crate::error::{Error, Result};
use crate::fd_fs::DirFd;
use crate::rollout::db_file;
use crate::secret::Allowlist;

mod capture;
mod classify;
mod restore;
mod tracker;

pub use restore::{
    activate, activate_preflight, refuse_if_inactive, restore_activate_disabled, restore_inactive,
};
pub(crate) use restore::{stage_inactive_members, verify_package_for_replace};
use restore::{table_present, validate_staged_store};
pub use tracker::admit_tracker;
use tracker::{
    admit_tracker_metadata, check_tracker_content, check_tracker_path, copy_tracker, git_bytes,
    git_plain, pm_root_of, Admission,
};

use classify::{
    check_tracker_private_content, classify_private_work, hex_digest, package_tracker_members,
    pm_root_from_paths,
};
pub const PORTABLE_FORMAT: &str = "cadence.portable/1";
pub const INACTIVE_MARKER: &str = "portable-restore.json";
pub(super) const PKG_DB: &str = "cadence.sqlite3";
const PKG_MANIFEST: &str = "manifest.json";
pub(super) const PKG_TRACKER: &str = "tracker";
/// Where the restored tracker lives inside a destination state dir.
pub(super) const TRACKER_DIR: &str = "portable-tracker";

/// Coordinator data carried by the coordination profile.
const COORDINATION: &[&str] = &[
    "schema_version",
    "agents",
    "agent_endpoints",
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
pub(super) const NULLED: &[(&str, &str)] = &[
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
pub(super) const DISABLED: &[(&str, &str)] = &[
    ("agents", "enabled"),
    ("monitors", "dispatch_enabled"),
    ("monitors", "auto_dispatch_enabled"),
    ("agent_endpoints", "verified"),
];

/// Text columns forced to a fixed safe value on the private copy and on
/// import. An imported endpoint must not accept mail until the operator
/// explicitly enables it again.
pub(super) const DISABLED_TEXT: &[(&str, &str, &str)] = &[("agent_endpoints", "state", "disabled")];

/// The fixed catalog of the store tables with their exact columns.
/// Export and restore refuse any table or column not listed here.
const CATALOG: &[(&str, &[&str])] = &[
    (
        "agent_endpoints",
        &[
            "id",
            "agent_uid",
            "kind",
            "slug",
            "state",
            "verified",
            "project",
            "workflow",
            "digest",
            "created",
            "updated",
        ],
    ),
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
            "install_id",
            "context_id",
            "source_post_read",
        ],
    ),
    (
        // CAD-2042: v62 backfill cursor. Empty once the upgrade finishes;
        // exported so a store is never refused for carrying its own table.
        "app_schema_progress",
        &["name", "cursor"],
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
            "agent_uid",
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
        "app_media_import_requests",
        &[
            "request_id",
            "asset_id",
            "source_receipt_id",
            "sha256",
            "result",
            "created",
        ],
    ),
    (
        "app_media_imports",
        &[
            "asset_id",
            "context_id",
            "source_receipt_id",
            "media_type",
            "sha256",
            "size_bytes",
            "bytes",
            "created",
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
        "app_media_lineage",
        &["asset_id", "source_asset_id", "source_sha256"],
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
        // CAD-2215: explicit per-member app grants, including revoked
        // tombstones. A row is the member's exact conversation binding, so
        // the pre-export private-work classification refuses any row; the
        // catalog entry exists so an empty table is never an unknown table.
        "app_member_grants",
        &[
            "org",
            "sub",
            "install_id",
            "conversation_id",
            "context_id",
            "install_digest",
            "state",
            "granted_at",
            "granted_by",
            "revoked_at",
            "revoked_by",
            "authorization_epoch",
        ],
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
        "workspace_settings",
        &["name", "value", "updated", "updated_by"],
    ),
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
        // CAD-2215: accepted member requests, including queued rows that no
        // ledger row has bound yet (`completed=0`). The row carries the
        // member, the request text and the planned job id, so the
        // pre-export private-work classification refuses any row.
        "assist_request_queue",
        &[
            "org",
            "sub",
            "request_key",
            "issue",
            "title",
            "body",
            "acceptance",
            "credential_id",
            "created",
            "last_attempt",
            "last_outcome",
            "completed",
            "job_id",
        ],
    ),
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
            "has_unresolved_pricing",
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
            "source_format",
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
        "connection_authority_changes",
        &["id", "table_name", "key_json"],
    ),
    (
        "connection_authority_rows",
        &["table_name", "key_json", "digest"],
    ),
    (
        "connection_dispatch_config",
        &[
            "singleton",
            "enabled",
            "ever_enabled",
            "fenced",
            "protection_required",
            "limits_json",
            "policy_digest",
        ],
    ),
    (
        "connection_dispatches",
        &[
            "dispatch_id",
            "request_id",
            "workspace_id",
            "custody_domain",
            "principal_key",
            "agent_uid",
            "connection_id",
            "credential_revision",
            "authorization_revision",
            "idempotency_key",
            "idempotency_digest",
            "payload_digest",
            "body_digest",
            "destination_digest",
            "commitment_digest",
            "commit_json",
            "committed_ms",
            "reserved_micros",
            "state",
            "actual_micros",
            "usage_json",
            "result_json",
            "error_code",
            "receipt_json",
            "settled_ms",
        ],
    ),
    (
        "connection_ledger",
        &["sequence", "database_epoch", "digest", "record_json"],
    ),
    (
        "connection_loaded_receipts",
        &[
            "loaded_receipt_id",
            "snapshot_id",
            "workspace_id",
            "manifest_digest",
            "observed_registry_digest",
            "runtime_generation",
            "enrollment_id",
            "receipt_json",
            "issued_ms",
            "expires_ms",
        ],
    ),
    (
        "connection_manifests",
        &[
            "manifest_id",
            "revision",
            "policy_digest",
            "material_digest",
            "manifest_json",
            "state",
        ],
    ),
    (
        "connection_metadata",
        &[
            "singleton",
            "workspace_id",
            "authorization_revision",
            "admission_clock_ms",
            "database_epoch",
            "ledger_sequence",
            "ledger_digest",
            "cursor_keyring_fingerprint",
        ],
    ),
    (
        "connection_model_quotes",
        &[
            "quote_id",
            "connection_id",
            "credential_revision",
            "authorization_revision",
            "principal_key",
            "process_activation",
            "quote_digest",
            "quote_json",
            "observed_ms",
            "expires_ms",
        ],
    ),
    (
        "connection_operation_policies",
        &[
            "connection_id",
            "credential_revision",
            "policy_revision",
            "policy_digest",
            "policy_json",
            "state",
        ],
    ),
    (
        "connection_policies",
        &[
            "connection_id",
            "provider",
            "visibility",
            "owner_user_id",
            "is_default",
            "write_mode",
            "allow_external_triggers",
            "allowed_agents_json",
            "policy_revision",
            "updated",
        ],
    ),
    (
        "connection_send_approvals",
        &[
            "approval_id",
            "principal_key",
            "connection_id",
            "credential_revision",
            "authorization_revision",
            "source_id",
            "source_digest",
            "recipient",
            "body",
            "approval_json",
            "consumed_dispatch_id",
        ],
    ),
    (
        "connection_snapshots",
        &[
            "snapshot_id",
            "workspace_id",
            "custody_domain",
            "agent_uid",
            "session_binding_id",
            "runtime_generation",
            "authorization_revision",
            "manifest_id",
            "manifest_revision",
            "manifest_digest",
            "policy_digest",
            "observed_registry_digest",
            "enrollment_id",
            "snapshot_json",
            "issued_ms",
            "expires_ms",
        ],
    ),
    (
        "credential_request_attempts",
        &[
            "attempt_id",
            "request_id",
            "nonce_digest",
            "session_binding",
            "verb",
            "state",
            "result_code",
            "claimed_at_ms",
            "finished_at_ms",
        ],
    ),
    (
        "credential_request_nonces",
        &[
            "nonce_digest",
            "request_id",
            "attempt_id",
            "session_binding",
            "request_revision",
            "provider",
            "verb",
            "connection_id",
            "target_id",
            "expected_credential_revision",
            "expected_authorization_revision",
            "custody_epoch",
            "context_digest",
            "issued_at_ms",
            "expires_at_ms",
            "claimed_at_ms",
        ],
    ),
    (
        "credential_request_resolutions",
        &[
            "request_id",
            "connection_id",
            "message_id",
            "conversation_id",
            "resumed_at",
        ],
    ),
    (
        "credential_requests",
        &[
            "request_id",
            "request_revision",
            "provider",
            "verb",
            "connection_id",
            "target_id",
            "enrollment_shape",
            "scopes",
            "expected_credential_revision",
            "expected_authorization_revision",
            "custody_domain",
            "custody_epoch",
            "context_digest",
            "requester_kind",
            "requester_participant",
            "conversation_id",
            "target_agent_uid",
            "state",
            "status",
            "result_code",
            "result_connection_id",
            "verification_request_id",
            "created_at_ms",
            "updated_at_ms",
            "expires_at_ms",
        ],
    ),
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
        "human_notification_prefs",
        &[
            "human", "dm", "mention", "sound", "preview", "quiet", "zone", "updated",
        ],
    ),
    ("human_notification_reads", &["room", "human", "seq"]),
    (
        "human_principals",
        &["id", "org", "issuer", "subject", "name", "seen"],
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
    (
        "human_push_devices",
        &[
            "id", "org", "issuer", "subject", "human", "endpoint", "p256dh", "auth", "session",
            "enrolled", "expires", "revoked",
        ],
    ),
    ("human_push_presence", &["device", "tab", "room", "expires"]),
    ("human_read_markers", &["room", "human", "seq"]),
    (
        "human_room_members",
        &["room", "kind", "member", "cutoff", "added", "removed"],
    ),
    ("human_room_mutes", &["room", "human", "muted"]),
    (
        "human_rooms",
        &[
            "id", "topic_id", "org", "kind", "pair", "title", "creator", "created",
        ],
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
            "stall_secs",
            "task_id",
            "priority",
            "issue",
            "worktree",
            "sender_uid",
            "recipient_uid",
            "sender",
            "recipient",
            "origin",
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
        "native_model_configurations",
        &[
            "configuration_id",
            "revision",
            "workspace_id",
            "agent_uid",
            "session_binding",
            "runtime_id",
            "runtime_generation",
            "owner_epoch",
            "role",
            "model",
            "model_uid",
            "effort",
            "provider",
            "source_id",
            "source_revision",
            "account",
            "origin",
            "destination",
            "billing_route",
            "sdk_graph_digest",
            "approved_tools_json",
            "manifest_digest",
            "snapshot_id",
            "snapshot_expires_ms",
            "loaded_receipt_id",
            "max_output_tokens",
            "created_ms",
        ],
    ),
    (
        "native_model_requests",
        &[
            "request_id",
            "workspace_id",
            "agent_uid",
            "durable_task_id",
            "request_index",
            "configuration_id",
            "configuration_revision",
            "snapshot_id",
            "loaded_receipt_id",
            "source_id",
            "account",
            "state",
            "input_digest",
            "assistant_json",
            "usage_json",
            "error_json",
            "billing_json",
            "auth_requests",
            "inference_requests",
            "created_ms",
            "committed_ms",
            "settled_ms",
        ],
    ),
    (
        "native_model_source_history",
        &["seq", "source_id", "revision", "action", "actor", "at_ms"],
    ),
    (
        "native_model_sources",
        &[
            "source_id",
            "workspace_id",
            "provider",
            "account",
            "revision",
            "origin",
            "origin_kind",
            "catalog_revision",
            "model_uid_map",
            "vault_reference",
            "key_id",
            "state",
            "created_ms",
            "updated_ms",
        ],
    ),
    (
        "native_model_tasks",
        &[
            "durable_task_id",
            "workspace_id",
            "agent_uid",
            "session_binding",
            "client_message_id",
            "server_message_id",
            "turn_token",
            "state",
            "created_ms",
            "fenced_ms",
            "fence_reason",
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
            "connection_operation_binding",
        ],
    ),
    (
        "platform_grants",
        &[
            "agent",
            "platform",
            "account",
            "scopes",
            "granted_at",
            "by",
            "agent_uid",
        ],
    ),
    (
        "protected_management_attempts",
        &[
            "nonce_digest",
            "attempt_id",
            "action",
            "params_digest",
            "session_binding",
            "custody_epoch",
            "issued_at_ms",
            "expires_at_ms",
            "claimed_at_ms",
            "state",
            "result_code",
            "finished_at_ms",
        ],
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
    (
        "routine_runs",
        &[
            "id",
            "routine_id",
            "due_instant",
            "trigger_source",
            "state",
            "approved_rev",
            "approved_sha256",
            "checklist_state_json",
            "script_output_json",
            "message_id",
            "error",
            "started_at",
            "finished_at",
        ],
    ),
    (
        "routines",
        &[
            "id",
            "name",
            "agent_alias",
            "trigger_kind",
            "cron",
            "interval_mins",
            "timezone",
            "event_source",
            "target_kind",
            "markdown_path",
            "approved_rev",
            "script_path",
            "approved_sha256",
            "destination_thread",
            "connection_account",
            "checklist_json",
            "inputs_json",
            "status",
            "pause_reason",
            "consecutive_failures",
            "next_due_at",
            "last_run_at",
            "last_run_state",
            "created",
            "updated",
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
        "staged_routine_proposals",
        &[
            "id",
            "kind",
            "proposed_by",
            "state",
            "title",
            "payload_json",
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
    /// The operator's explicit claim that no unidentified chat-attachment
    /// custody root holds private bytes for this source. Proving that from the
    /// store cannot be done by this process — the daemon resolves the root
    /// from its in-process provider environment and persists no binding — so
    /// an otherwise-clean source refuses unless the operator states the fact.
    /// A source that does hold a classifiable private record refuses anyway.
    pub attest_chat_custody_absent: bool,
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
    // CAD-2215: refuse a workspace that evidences private work BEFORE any
    // snapshot copy of its store is written. `build_package` repeats this on
    // the snapshot, which is the authoritative copy it would project.
    classify_private_work(
        &live,
        state_dir,
        &opts.tracker,
        &opts.tracker,
        &head,
        opts.attest_chat_custody_absent,
    )?;
    let staging = private_staging(out)?;
    let manifest = match build_package(
        state_dir,
        &opts.tracker,
        &live,
        &staging,
        &opts.tracker,
        &head,
        &allow,
        opts,
    ) {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    // CAD-2215: the store snapshot, the pinned tracker tree and the private
    // delivery ledger are three sources, not one atomic snapshot. The
    // package's own bytes were classified above; re-read the SOURCE now that
    // it is complete, so private work that appeared while it was packaged
    // refuses publication instead of being published outside the
    // classification that admitted it. Private content that appears after
    // this check is not in the package at all.
    if let Err(error) = classify_private_work(
        &live,
        state_dir,
        &opts.tracker,
        &opts.tracker,
        &head,
        opts.attest_chat_custody_absent,
    ) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if let Err(error) = reverify_tracker(&opts.tracker, &head) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    publish(&staging, out, &manifest)
}

/// CAD-2215: re-prove, after the package is built, that the source tracker
/// is still the clean revision the package pinned. The package carries only
/// `head`'s tree, so a change here cannot make it unsafe — it makes the
/// receipt name a revision the source has moved past, which is refused
/// rather than published. The snapshot export instead re-stamps the whole
/// source through `TrackerCapture::verify_unchanged`.
fn reverify_tracker(tracker: &Path, head: &str) -> Result<()> {
    let now = git_plain(tracker, &["rev-parse", "--verify", "HEAD"])?;
    if now != head {
        return Err(Error::rejected(
            "portable export refused: the source tracker moved while the package was being \
             built; no package was written and the source is unchanged",
        ));
    }
    let status = git_plain(
        tracker,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !status.trim().is_empty() {
        return Err(Error::rejected(
            "portable export refused: the source tracker changed while the package was being \
             built; no package was written and the source is unchanged",
        ));
    }
    Ok(())
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
    // CAD-2215: refuse before the store snapshot is written, classifying the
    // live store and the same captured tracker `build_package` would copy.
    classify_private_work(
        &live,
        state_dir,
        &opts.tracker,
        captured.path(),
        &head,
        opts.attest_chat_custody_absent,
    )?;
    let staging = private_staging(out)?;
    let result = (|| {
        let manifest = build_package(
            state_dir,
            &opts.tracker,
            &live,
            &staging,
            captured.path(),
            &head,
            &allow,
            opts,
        )?;
        let mut report = inspect_portable(&staging)?;
        captured.verify_package(&staging, &manifest.members)?;
        captured.verify_unchanged()?;
        report["source_capture"] = captured.evidence();
        report["exporter_revision"] = json!(crate::overview::BUILD_COMMIT);
        report["source_fidelity"] = json!("captured_tracker_matches_package_and_rechecked");
        report["atomic_database_and_tracker"] = json!(false);
        report["bundle"] = json!(out);
        // CAD-2215: same three-source window as the ordinary export. The
        // package bytes were classified in `build_package`; re-read the LIVE
        // store and private ledger now that the package is complete, so
        // private work that appeared while packaging refuses publication.
        // `verify_unchanged` above separately re-proves the source tracker
        // still matches the private capture.
        classify_private_work(
            &live,
            state_dir,
            &opts.tracker,
            captured.path(),
            &head,
            opts.attest_chat_custody_absent,
        )?;
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
    /// The private copy every verified member was written to and re-read from.
    /// Consumers must open or stream members from here, never from the mutable
    /// caller-supplied package.
    snapshot: tempfile::TempDir,
}

impl VerifiedPackage {
    /// Root of the private verified snapshot. Same-UID processes are not
    /// isolated from it.
    pub fn snapshot_root(&self) -> &Path {
        self.snapshot.path()
    }
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

/// Manifest structure checks that read no member: format/scope, the admitted
/// PM root, every member path, the exact PM root derived from the member
/// inventory, and the presence of the core store. Shared by every verifier so
/// inspection, transfer, activation and restore admit the same shape.
fn admit_manifest(manifest_bytes: &[u8]) -> Result<PortableManifest> {
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
    // CAD-2215: the PM root is decided by the MEMBER INVENTORY that is about
    // to be verified, never by the manifest's own field. A manifest that says
    // `null` while its tracker carries `pm.yaml` would otherwise skip every
    // project record in the package.
    let tracker_prefix = format!("{PKG_TRACKER}/");
    let mut seen = std::collections::BTreeSet::new();
    for m in &manifest.members {
        let rel = Path::new(&m.path);
        if !safe_relative(rel) {
            return Err(Error::rejected(format!("unsafe member path {}", m.path)));
        }
        if m.path != PKG_DB && !m.path.starts_with(&tracker_prefix) {
            return Err(Error::rejected(format!(
                "member {} is outside the core store and tracker",
                m.path
            )));
        }
        if !seen.insert(m.path.clone()) {
            return Err(Error::rejected(format!("duplicate member {}", m.path)));
        }
    }
    if !seen.contains(PKG_DB) {
        return Err(Error::rejected("package has no store member"));
    }
    let tracker_paths: Vec<&str> = manifest
        .members
        .iter()
        .filter_map(|m| m.path.strip_prefix(&tracker_prefix))
        .collect();
    if pm_root_from_paths(tracker_paths.iter().copied())? != manifest.pm_root {
        return Err(Error::rejected(
            "package manifest pm_root does not match its tracker members",
        ));
    }
    Ok(manifest)
}

/// Copy every manifest member into a fresh 0700 private directory, verifying
/// and classifying the SAME bytes it writes. Inspection, activation preflight
/// and remote transfer then read only this snapshot, so a later change to the
/// mutable source package cannot be validated as clean and then streamed or
/// planned as private.
fn snapshot_members(root: &PackageRoot, manifest: &PortableManifest) -> Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new()
        .prefix("cadence-verified-")
        .tempdir()?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
    let prefix = format!("{PKG_TRACKER}/");
    for m in &manifest.members {
        let rel = Path::new(&m.path);
        let dst = dir.path().join(rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        if m.path == PKG_DB {
            // The store can be large: hash and copy in one streamed pass,
            // refusing the moment the streamed bytes disagree.
            stream_member(root.open_member(rel)?, &dst, m)?;
            continue;
        }
        // One read through the pinned descriptor: the bytes hashed are the
        // bytes classified and the bytes written into the snapshot.
        let bytes = read_member(root, rel, m.bytes)?;
        if hex_digest(&bytes) != m.sha256 || bytes.len() as u64 != m.bytes {
            return Err(Error::rejected(format!(
                "member {} changed (digest or size mismatch)",
                m.path
            )));
        }
        if let Some(tracked) = m.path.strip_prefix(&prefix) {
            check_tracker_path(tracked)?;
            check_tracker_content(tracked, &bytes)?;
            check_tracker_private_content("package", manifest.pm_root.as_deref(), tracked, &bytes)?;
        }
        write_private_mode(&dst, &bytes, 0o600)?;
    }
    Ok(dir)
}

/// Hash-and-copy one large member in a single pass into the private snapshot.
/// `input` is already an opened, fstat-checked member of the pinned root.
fn stream_member(mut input: std::fs::File, dst: &Path, m: &Member) -> Result<()> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dst)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > m.bytes {
            return Err(Error::rejected(format!(
                "member {} changed (digest or size mismatch)",
                m.path
            )));
        }
        hasher.update(&buf[..n]);
        output.write_all(&buf[..n])?;
    }
    if total != m.bytes || format!("{:x}", hasher.finalize()) != m.sha256 {
        return Err(Error::rejected(format!(
            "member {} changed (digest or size mismatch)",
            m.path
        )));
    }
    output.sync_all()?;
    Ok(())
}

/// A descriptor-pinned package directory. Every manifest and member read walks
/// from this descriptor with `openat(O_NOFOLLOW)`, and the leaf is fstat'ed on
/// the opened inode, so a symlink swapped into the leaf or any ancestor after
/// the pin cannot redirect a read outside the package. The package may be
/// owned by a different unprivileged UID, so path-based checks are not enough.
struct PackageRoot {
    dir: DirFd,
}

impl PackageRoot {
    fn open(root: &Path) -> Result<Self> {
        // Traverse the caller's spelling component by component with pinned,
        // no-follow directory handles. `..` returns to the pinned parent only
        // after the preceding component was itself opened as a real
        // directory, so `a/../b` keeps POSIX traversal semantics and a
        // symlinked or missing `a` refuses instead of being skipped as text.
        // `canonicalize` is never used: it would follow a swapped ancestor.
        let mut stack: Vec<DirFd> = if root.is_absolute() {
            vec![DirFd::open_root()?]
        } else {
            vec![DirFd::open_nofollow(Path::new("."))?]
        };
        for component in root.components() {
            match component {
                std::path::Component::RootDir => {
                    stack.truncate(1);
                    stack[0] = DirFd::open_root()?;
                }
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if stack.len() > 1 {
                        stack.pop();
                    } else {
                        stack[0] = stack[0].open_child_dir(c"..").map_err(|_| {
                            Error::rejected("package parent directory is unavailable")
                        })?;
                    }
                }
                std::path::Component::Normal(name) => {
                    let encoded = CString::new(name.as_bytes())
                        .map_err(|_| Error::rejected("package path has NUL"))?;
                    let child = stack
                        .last()
                        .expect("the stack starts with a root handle")
                        .open_child_dir(&encoded)
                        .map_err(|_| {
                            Error::rejected(format!(
                                "package {} is not reachable through real directories",
                                root.display()
                            ))
                        })?;
                    stack.push(child);
                }
                std::path::Component::Prefix(_) => {
                    return Err(Error::rejected("unsupported package path prefix"))
                }
            }
        }
        let current = stack
            .pop()
            .ok_or_else(|| Error::rejected("package path is empty"))?;
        if current.stat()?.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(Error::rejected("package is not a plain directory"));
        }
        Ok(Self { dir: current })
    }

    /// Open one package-relative member through the pinned descriptor.
    fn open_member(&self, rel: &Path) -> Result<std::fs::File> {
        let mut parts = Vec::new();
        for component in rel.components() {
            match component {
                std::path::Component::Normal(name) => parts.push(name.to_owned()),
                _ => return Err(Error::rejected("unsafe package member path")),
            }
        }
        let Some((leaf, dirs)) = parts.split_last() else {
            return Err(Error::rejected("empty package member path"));
        };
        let mut dir = self.dir.try_clone()?;
        for name in dirs {
            let encoded = CString::new(name.as_bytes())
                .map_err(|_| Error::rejected("package member path has NUL"))?;
            dir = dir.open_child_dir(&encoded).map_err(|_| {
                Error::rejected(format!(
                    "package member {} is not reachable through a real directory",
                    rel.display()
                ))
            })?;
        }
        let encoded =
            CString::new(leaf.as_bytes()).map_err(|_| Error::rejected("member path has NUL"))?;
        let file = dir.open_readonly_nofollow(&encoded).map_err(|_| {
            Error::rejected(format!(
                "package member {} is not a plain readable file",
                rel.display()
            ))
        })?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.nlink() != 1 {
            return Err(Error::rejected(format!(
                "{} is not a plain single-link regular file",
                rel.display()
            )));
        }
        Ok(file)
    }
}

/// Every package-relative regular file, walked through pinned directory
/// handles. Each directory is enumerated via `fdopendir` on a duplicated
/// descriptor and each step descends with `openat(O_NOFOLLOW)`, so no path
/// component can be swapped mid-walk on either Linux or macOS.
#[cfg(unix)]
fn package_paths(root: &PackageRoot) -> Result<Vec<String>> {
    fn walk(dir: &DirFd, prefix: &str, out: &mut Vec<String>) -> Result<()> {
        let entries = dir
            .read_dir()
            .map_err(|_| Error::rejected("cannot enumerate a package directory"))?;
        for entry in entries {
            let encoded =
                entry.map_err(|_| Error::rejected("cannot enumerate a package directory"))?;
            let stat = dir
                .fstatat_nofollow(&encoded)
                .map_err(|_| Error::rejected("package entry cannot be classified"))?;
            let rel = format!("{prefix}{}", String::from_utf8_lossy(encoded.as_bytes()));
            match stat.st_mode & libc::S_IFMT {
                libc::S_IFLNK => {
                    return Err(Error::rejected("package contains a symlink"));
                }
                libc::S_IFDIR => {
                    let child = dir
                        .open_child_dir(&encoded)
                        .map_err(|_| Error::rejected("package directory is not reachable"))?;
                    walk(&child, &format!("{rel}/"), out)?;
                }
                libc::S_IFREG => out.push(rel),
                _ => {
                    return Err(Error::rejected("package contains a non-regular file"));
                }
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(&root.dir, "", &mut out)?;
    out.sort();
    Ok(out)
}

/// Read a pinned member fully.
fn read_member(root: &PackageRoot, rel: &Path, limit: u64) -> Result<Vec<u8>> {
    // Check the opened inode's size before allocating, and never read more
    // than the declared length: a sparse or oversized source cannot make this
    // allocate beyond the manifest's own bound.
    let file = root.open_member(rel)?;
    if file.metadata()?.len() != limit {
        return Err(Error::rejected(format!(
            "member {} changed (digest or size mismatch)",
            rel.display()
        )));
    }
    let mut bytes = Vec::with_capacity(limit.min(1024 * 1024) as usize);
    let read = file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if read as u64 != limit {
        return Err(Error::rejected(format!(
            "member {} changed (digest or size mismatch)",
            rel.display()
        )));
    }
    Ok(bytes)
}

/// The manifest itself, capped before allocation.
fn read_manifest_file(root: &PackageRoot) -> Result<Vec<u8>> {
    let file = root.open_member(Path::new(PKG_MANIFEST))?;
    let declared = file.metadata()?.len();
    if declared > TRANSFER_MANIFEST_MAX as u64 {
        return Err(Error::rejected(
            "package manifest exceeds the transfer limit",
        ));
    }
    let mut bytes = Vec::with_capacity(declared as usize);
    let read = file
        .take(TRANSFER_MANIFEST_MAX as u64 + 1)
        .read_to_end(&mut bytes)?;
    if read as u64 != declared {
        return Err(Error::rejected("package manifest changed while being read"));
    }
    Ok(bytes)
}

/// Hash the manifest through the pinned package root without admitting its contents.
pub(crate) fn package_manifest_digest(source: &Path) -> Result<String> {
    let root = PackageRoot::open(source)?;
    Ok(hex_digest(&read_manifest_file(&root)?))
}

/// Hosted transfer bounds, enforced before a snapshot is created so a hostile
/// manifest cannot make inspection copy or allocate beyond them.
fn enforce_transfer_bounds(manifest: &PortableManifest) -> Result<()> {
    if manifest.members.len() > TRANSFER_MEMBERS_MAX {
        return Err(Error::rejected(
            "package member count exceeds the transfer limit",
        ));
    }
    let mut total = 0u64;
    for member in &manifest.members {
        if member.bytes > TRANSFER_MEMBER_MAX_BYTES {
            return Err(Error::rejected(
                "package member exceeds the transfer byte limit",
            ));
        }
        if member.path.encode_utf16().count() > TRANSFER_MEMBER_PATH_MAX_UNITS {
            return Err(Error::rejected(
                "package member path exceeds the transfer limit",
            ));
        }
        total = total
            .checked_add(member.bytes)
            .ok_or_else(|| Error::rejected("package byte total overflows"))?;
    }
    if total > TRANSFER_INVENTORY_MAX_BYTES {
        return Err(Error::rejected(
            "package byte total exceeds the transfer limit",
        ));
    }
    Ok(())
}

pub(super) fn inspect_package(source: &Path) -> Result<VerifiedPackage> {
    let root = PackageRoot::open(source)?;
    // Reject linked directories before reading any listed member through them.
    // (Member reads walk the pinned descriptor below, so this only enumerates
    // names for the unlisted-member check.)
    let paths = package_paths(&root)?;
    let manifest_bytes = read_manifest_file(&root)?;
    let manifest_sha256 = format!("{:x}", Sha256::digest(&manifest_bytes));
    let manifest = admit_manifest(&manifest_bytes)?;
    // Hosted transfer bounds are enforced before the snapshot copies or
    // allocates anything.
    enforce_transfer_bounds(&manifest)?;
    // The manifest's member set must be exactly what the package holds (plus
    // the manifest itself); an unlisted file could carry content no
    // classification saw.
    for path in &paths {
        if path != PKG_MANIFEST && !manifest.members.iter().any(|m| &m.path == path) {
            return Err(Error::rejected(format!(
                "package contains unlisted member {path}"
            )));
        }
    }
    // Verify and classify each member while copying it into a private
    // snapshot, then use that snapshot rather than the mutable source.
    // This does not isolate the copy from other processes with the same UID.
    let snapshot = snapshot_members(&root, &manifest)?;
    let snapshot_db = snapshot.path().join(PKG_DB);
    // Ordinary read-only SQLite opens can still create WAL/SHM files. Native
    // exports are rollback-journal single-file databases; admit that header
    // before any SQLite connection touches the snapshot.
    let mut header = [0u8; 20];
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&snapshot_db)?
        .read_exact(&mut header)?;
    if &header[..16] != b"SQLite format 3\0" || header[18] != 1 || header[19] != 1 {
        return Err(Error::rejected(
            "portable inspection requires a single-file rollback-journal SQLite database",
        ));
    }
    validate_staged_store(&snapshot_db, &manifest)?;
    let snapshot_tracker = snapshot.path().join(PKG_TRACKER);
    if pm_root_of(&snapshot_tracker)? != manifest.pm_root {
        return Err(Error::rejected(
            "package tracker PM root differs from manifest",
        ));
    }
    if hex_digest(&read_manifest_file(&root)?) != manifest_sha256 {
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
        snapshot,
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

#[allow(clippy::too_many_arguments)] // the package inputs are positional by design
fn build_package(
    state_dir: &Path,
    workspace: &Path,
    live: &Path,
    staging: &Path,
    tracker: &Path,
    head: &str,
    allow: &Allowlist,
    opts: &PortableExportOptions,
) -> Result<PortableManifest> {
    let mut output = ArtifactDir::open(staging, false)?;
    let temp_name = format!(".{PKG_DB}.partial");
    let partial = output.file_path(temp_name.as_ref())?;
    // Private online snapshot; the live store is only read.
    artifact_snapshot(live, &mut output, temp_name.as_ref())?;
    let source_uuid = read_source_uuid(&partial)?;
    check_catalog(&partial)?;
    // CAD-2215: prove the source holds no private member work BEFORE the
    // projection touches this copy and before any tracker blob is copied.
    // A refusal here leaves the staging dir to the caller's cleanup, never
    // a package: the source store, tracker and state dir are only read.
    classify_private_work(
        &partial,
        state_dir,
        workspace,
        tracker,
        head,
        opts.attest_chat_custody_absent,
    )?;
    output.sqlite_sidecars(temp_name.as_ref())?;
    let omitted_rows = project(&partial)?;
    // Existing redaction on the private copy, then a read-only secret scan
    // and integrity check of the projected copy.
    output.sqlite_sidecars(temp_name.as_ref())?;
    let scrub = scrub(&partial, opts.redact_history_secrets)?;
    scan_db(&partial, allow)?;
    let (integrity, schema) = inspect(&partial)?;
    require_ok(&partial, &integrity)?;
    output.clean_sidecars(temp_name.as_ref())?;
    output.publish(temp_name.as_ref(), PKG_DB.as_ref())?;
    let db = output.file_path(PKG_DB.as_ref())?;
    let tracker_dir = staging.join(PKG_TRACKER);
    copy_tracker(tracker, &tracker_dir, head)?;
    let pm_root = pm_root_of(&tracker_dir)?;
    // CAD-2215: build every tracker member from ONE read, hashing and
    // classifying that same buffer. The bytes recorded in the manifest are
    // therefore exactly the bytes classification admitted, and a concurrent
    // swap between a classified copy and a hashed one cannot occur.
    let tracker_members = package_tracker_members(&tracker_dir, pm_root.as_deref())?;
    let mut members = vec![member_for(&db, PKG_DB)?];
    members.extend(tracker_members);
    let mut package_exclusions = exclusions();
    if opts.redact_history_secrets {
        package_exclusions.push(
            "credential-shaped spans in UTF-8 text messages.body (opt-in; includes warn-only findings)"
                .into(),
        );
    }
    if opts.attest_chat_custody_absent {
        package_exclusions.push(
            "chat attachment custody outside the store, tracker and state directory (operator \
             asserted absent for this source; this package does not carry it)"
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
        disabled_columns: DISABLED
            .iter()
            .map(|(t, c)| format!("{t}.{c}"))
            .chain(DISABLED_TEXT.iter().map(|(t, c, _)| format!("{t}.{c}")))
            .collect(),
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
pub(super) fn omitted_names() -> Vec<&'static str> {
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
pub(super) fn check_catalog(db: &Path) -> Result<()> {
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

pub(super) fn sql_string(s: &str) -> String {
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
    // Classification already refuses a source that holds human rooms; this is
    // defence in depth for a copy that somehow still carries one. Scrub those
    // rows before the room metadata is dropped, so no private human post,
    // delivery or membership leaks through the coordination profile.
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
    // `events`). Classification already refuses a source with any cloud-agent
    // marker (tables, alias provider or `source='cloud_agent'` message), so
    // the closure below is defence in depth for a copy that somehow still
    // carries one. Exporting that text against an alias whose authority rows
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
        if !table_present(&tx, table)? {
            continue;
        }
        tx.execute(&format!("UPDATE {table} SET {column}=0"), [])?;
    }
    for (table, column, value) in DISABLED_TEXT {
        if !table_present(&tx, table)? {
            continue;
        }
        tx.execute(
            &format!("UPDATE {table} SET {column}=?1"),
            rusqlite::params![value],
        )?;
    }
    tx.commit()?;
    conn.execute_batch("VACUUM")?;
    Ok(omitted)
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn member_for(file: &Path, rel: &str) -> Result<Member> {
    let (sha256, bytes) = hash_file(file)?;
    Ok(Member {
        path: rel.to_string(),
        bytes,
        sha256,
    })
}

pub(super) fn hash_file(path: &Path) -> Result<(String, u64)> {
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

pub(super) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
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
pub(super) fn rename_noreplace(src: &Path, dst: &Path) -> Result<()> {
    let s =
        CString::new(src.as_os_str().as_bytes()).map_err(|_| Error::rejected("path has NUL"))?;
    let d =
        CString::new(dst.as_os_str().as_bytes()).map_err(|_| Error::rejected("path has NUL"))?;
    // SAFETY: `s` and `d` are live NUL-terminated C strings passed to
    // `renameat2(AT_FDCWD, ..., RENAME_NOREPLACE)`.
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
pub(super) fn rename_noreplace(_src: &Path, _dst: &Path) -> Result<()> {
    Err(Error::rejected(NO_ATOMIC_PUBLISH))
}

fn exclusions() -> Vec<String> {
    [
        "authority: platform_credentials, platform_grants, platform_defaults, platform_drafts, platform_effects, app_grants, rollout_grants, rollout_leases, staging_grants, staging_instances, daemon_build (rows deleted from the private copy)",
        "private or private-capable state is never exported at any fidelity, and a source holding any of it refuses export instead of dropping it: queued-but-unpromoted member requests (assist_request_queue), member-private job ownership (assist_requests, assist_job_shares, assist_stall_checks), member job subscriptions (job_subscribers), member assistants (assistants), app member grants in any state including revoked tombstones (app_member_grants), app conversations (threads.install_id), app contexts (app_contexts), app team defaults (app-teams.json), per-installation app records (app-records/), chat attachments (chat_files, chat-file custody under the workspace .cadence/chat-files and the state chat-files/), chat upload accounts (chat_upload_accounts), issue artifacts (artifacts, artifacts/), member-private job specs (assist_jobs/), upload staging under wiki-uploads/, human-room topics and membership (human_rooms, human_room_members, human_room_mutes, topics.type=human_room), cloud-agent state (cloud_agents, cloud_agent_sources, cloud_agent_sessions, cloud_agent_operations, provider=agenticos-cloud aliases, source=cloud_agent messages), the state-dir private delivery/publication ledger (delivery-private.json), publication comments in the tracker, and private project policies (project.yaml visibility private). This package was classified as holding none of it",
        "live execution authority nulled: agents.generation, pid, pid_start, endpoint, thread_id, session_id, params; messages.turn_id; token-shaped values redacted by the existing scrub",
        "imported actors disabled: agents.enabled=0; monitors dispatch flags=0",
        "provenance purged: connection_metadata (source UUID recorded in the manifest only), model_defaults, settings_authority",
        "other app/business data outside the coordination profile and not classified above: app_installations, app_bindings, app_runs, app media/assets/records/sends, crm_*, social_publish_intents, cloud_dispatch_outbox (rows deleted from the private copy; declared omitted, not a full workspace)",
        "residual cloud-agent and human-room rows: the projection still deletes any it finds as defence in depth, but export already refuses a source holding cloud-agent or human-room state, so a published package carries none",
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
