//! CAD-1853: the durable cloud-agent target, per-source provider-session
//! bindings, explicit per-source consent and one operation row per request.
//!
//! The remote cloud agent is ONE stable tenant chat target: its identity does
//! not change with the caller. The contract separately derives the invoker's
//! provenance (the human user, or one enrolled local peer) and a provider
//! session per `(target, source, canonical conversation)`. Native therefore
//! keys every session by `(cloud_agent, source_kind, source_uid,
//! registration_epoch, thread)` and pins each returned session: one source
//! never overwrites, adopts or reads another source's provider binding.
//!
//! Canonical work history stays in Threads (`thread_entries`). Rows here hold
//! only correlation and consent.

use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{now, Store, StoreConn};
use crate::error::{Error, Result};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS cloud_agents(
    id TEXT PRIMARY KEY,
    alias TEXT NOT NULL UNIQUE,
    organization_id TEXT NOT NULL,
    org_slug TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    remote_agent_id TEXT NOT NULL,
    runtime TEXT NOT NULL,
    model TEXT,
    capabilities TEXT NOT NULL,
    provider_session_id TEXT,
    provenance TEXT,
    recipient_uid TEXT NOT NULL,
    consent_scope TEXT NOT NULL,
    state TEXT NOT NULL,
    created REAL NOT NULL,
    updated REAL NOT NULL,
    UNIQUE(endpoint, organization_id, remote_agent_id));
CREATE TABLE IF NOT EXISTS cloud_agent_sources(
    cloud_agent_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_uid TEXT NOT NULL,
    registration_epoch INTEGER NOT NULL,
    owner_issuer TEXT NOT NULL,
    owner_subject TEXT NOT NULL,
    label TEXT NOT NULL,
    scope TEXT NOT NULL,
    granted_by TEXT NOT NULL,
    granted_at REAL NOT NULL,
    invoker_id TEXT,
    state TEXT NOT NULL,
    PRIMARY KEY(cloud_agent_id, source_kind, source_uid, registration_epoch,
                owner_issuer, owner_subject));
CREATE TABLE IF NOT EXISTS cloud_agent_sessions(
    id TEXT PRIMARY KEY,
    cloud_agent_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_uid TEXT NOT NULL,
    registration_epoch INTEGER NOT NULL,
    owner_issuer TEXT NOT NULL,
    owner_subject TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    remote_session_id TEXT NOT NULL,
    provider_session_id TEXT,
    invoker_id TEXT,
    created REAL NOT NULL,
    UNIQUE(cloud_agent_id, source_kind, source_uid, registration_epoch,
           owner_issuer, owner_subject, thread_id),
    UNIQUE(cloud_agent_id, source_kind, source_uid, registration_epoch,
           owner_issuer, owner_subject, conversation_id),
    UNIQUE(cloud_agent_id, remote_session_id));
CREATE TABLE IF NOT EXISTS cloud_agent_operations(
    operation_id TEXT NOT NULL,
    cloud_agent_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_uid TEXT NOT NULL,
    registration_epoch INTEGER NOT NULL,
    owner_issuer TEXT NOT NULL,
    owner_subject TEXT NOT NULL,
    recipient_uid TEXT NOT NULL,
    sender_label TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    remote_session_id TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    request_text TEXT NOT NULL,
    status TEXT NOT NULL,
    remote_operation_id TEXT,
    message_id TEXT,
    reply_text TEXT,
    reply_message_id TEXT,
    error_code TEXT,
    error_message TEXT,
    created REAL NOT NULL,
    updated REAL NOT NULL,
    grant_admitted_at REAL NOT NULL DEFAULT 0,
    claimed_at REAL NOT NULL DEFAULT 0,
    PRIMARY KEY(cloud_agent_id, operation_id));
CREATE INDEX IF NOT EXISTS cloud_agent_operations_thread
    ON cloud_agent_operations(cloud_agent_id, thread_id, created);
CREATE INDEX IF NOT EXISTS cloud_agent_operations_status
    ON cloud_agent_operations(cloud_agent_id, status);
";

/// The local explicit consent a paid cloud turn needs for the source. It is
/// never implied by a saved hosted login's `cli.write`; the matching
/// server-side contract scope is `cloud_agent.write`.
pub const CONSENT_SCOPE: &str = "cloud_agent.write";

pub const STATE_CONNECTED: &str = "connected";
pub const STATE_DISCONNECTED: &str = "disconnected";

pub const SOURCE_USER: &str = "user";
pub const SOURCE_PEER: &str = "local_peer";

pub const SOURCE_ACTIVE: &str = "active";
pub const SOURCE_REVOKED: &str = "revoked";

/// The FULL stable logical owner a cloud source belongs to.
///
/// A native UID + registration stamp is not enough on its own: the same peer
/// row can be re-enrolled under another human, and the same human login can be
/// replaced by a different subject at the same tenant. Both halves of the
/// issuer-verified parent identity therefore key every source, provider
/// session, operation witness and peer credential: a replaced owner starts
/// with no inherited grant, session or cached reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub issuer: String,
    pub subject: String,
}

impl Owner {
    pub fn new(issuer: impl Into<String>, subject: impl Into<String>) -> Owner {
        Owner {
            issuer: issuer.into(),
            subject: subject.into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.issuer.trim().is_empty() || self.subject.trim().is_empty()
    }

    pub fn to_json(&self) -> Value {
        json!({"issuer": self.issuer, "subject": self.subject})
    }
}

pub const STATUS_QUEUED: &str = "queued";
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_PARKED: &str = "parked";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_UNKNOWN: &str = "unknown";

pub const STATUSES: &[&str] = &[
    STATUS_QUEUED,
    STATUS_RUNNING,
    STATUS_PARKED,
    STATUS_COMPLETED,
    STATUS_FAILED,
    STATUS_UNKNOWN,
];

/// Terminal statuses. A terminal row is never re-invoked; `unknown` is not
/// terminal — it is resolved only by readback.
pub fn settled(status: &str) -> bool {
    matches!(status, STATUS_COMPLETED | STATUS_FAILED)
}

/// One connected cloud-agent target.
#[derive(Debug, Clone)]
pub struct CloudAgent {
    pub id: String,
    pub alias: String,
    pub organization_id: String,
    pub org_slug: String,
    pub endpoint: String,
    pub remote_agent_id: String,
    pub runtime: String,
    pub model: Option<String>,
    pub capabilities: Value,
    pub provider_session_id: Option<String>,
    pub provenance: Value,
    /// The local `agents.uid` this connection was created against. The
    /// canonical request is enqueued to that exact registration, never to
    /// whatever the alias happens to resolve to later.
    pub recipient_uid: String,
    pub consent_scope: String,
    pub state: String,
    pub created: f64,
    pub updated: f64,
}

impl CloudAgent {
    pub fn connected(&self) -> bool {
        self.state == STATE_CONNECTED
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "alias": self.alias,
            "kind": "cloud",
            "organization_id": self.organization_id,
            "org": self.org_slug,
            "endpoint": self.endpoint,
            "remote_agent_id": self.remote_agent_id,
            "runtime": self.runtime,
            "model": self.model,
            "capabilities": self.capabilities,
            "provider_session_id": self.provider_session_id,
            "provenance": self.provenance,
            "recipient_uid": self.recipient_uid,
            "consent_scope": self.consent_scope,
            "state": self.state,
            "connected": self.connected(),
            "local_execution": false,
            "shell": false,
            "git": false,
            "effects": false,
            "cancellation": false,
            "created": self.created,
            "updated": self.updated,
        })
    }
}

/// One explicitly granted invoker source: the human user, or one enrolled
/// local peer pinned to its immutable local UID + registration epoch.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudSource {
    pub cloud_agent_id: String,
    pub kind: String,
    pub uid: String,
    pub registration_epoch: i64,
    pub owner: Owner,
    pub label: String,
    pub scope: String,
    pub granted_by: String,
    pub granted_at: f64,
    pub invoker_id: Option<String>,
    pub state: String,
}

impl CloudSource {
    pub fn active(&self) -> bool {
        self.state == SOURCE_ACTIVE
    }

    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "uid": self.uid,
            "registration_epoch": self.registration_epoch,
            "owner": self.owner.to_json(),
            "label": self.label,
            "scope": self.scope,
            "invoker_id": self.invoker_id,
            "state": self.state,
            "granted_by": self.granted_by,
            "granted_at": self.granted_at,
        })
    }
}

/// One local thread's remote provider session, under exactly one source.
#[derive(Debug, Clone)]
pub struct CloudSession {
    pub id: String,
    pub cloud_agent_id: String,
    pub source_kind: String,
    pub source_uid: String,
    pub registration_epoch: i64,
    pub owner: Owner,
    pub thread_id: String,
    pub conversation_id: String,
    pub remote_session_id: String,
    pub provider_session_id: Option<String>,
    pub invoker_id: Option<String>,
    pub created: f64,
}

impl CloudSession {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "cloud_agent_id": self.cloud_agent_id,
            "source": {
                "kind": self.source_kind,
                "uid": self.source_uid,
                "registration_epoch": self.registration_epoch,
                "owner": self.owner.to_json(),
            },
            "thread_id": self.thread_id,
            "conversation_id": self.conversation_id,
            "remote_session_id": self.remote_session_id,
            "provider_session_id": self.provider_session_id,
            "invoker_id": self.invoker_id,
            "created": self.created,
        })
    }
}

/// One row on an ordinary alias whose immutable origin provenance reaches a
/// cloud request/result, with the UID binding the delivery carried.
#[derive(Debug, Clone)]
pub struct CloudDerivedRow {
    pub message_id: String,
    pub sender_uid: Option<String>,
    pub recipient_uid: Option<String>,
}

/// One durable request ⇄ remote operation correlation.
#[derive(Debug, Clone)]
pub struct CloudOperation {
    pub operation_id: String,
    pub cloud_agent_id: String,
    pub source_kind: String,
    pub source_uid: String,
    pub registration_epoch: i64,
    pub owner: Owner,
    pub recipient_uid: String,
    pub sender_label: String,
    pub thread_id: String,
    pub conversation_id: String,
    pub remote_session_id: String,
    pub request_digest: String,
    pub request_text: String,
    pub status: String,
    pub remote_operation_id: Option<String>,
    pub message_id: Option<String>,
    pub reply_text: Option<String>,
    pub reply_message_id: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created: f64,
    pub updated: f64,
    /// The `cloud_agent_sources.granted_at` lineage this operation was
    /// ADMITTED under, copied at admission. A wake must present that same
    /// grant still current: a consent that was revoked and later regranted
    /// for the same stable owner is a different lineage and may not revive
    /// this request — a new operation id is the supported path.
    pub grant_admitted_at: f64,
    /// When this operation's submission was last claimed across the
    /// transport boundary, or `0` while it has never been claimed. The claim
    /// is the durable submission eligibility witness: an operation that has
    /// ever been claimed can never be claimed again, even if the contract's
    /// own answer leaves the row `queued` (an accepted-queued response
    /// without a remote operation id). A row that never reached the
    /// transport is still `queued` with `claimed_at = 0` and may be retried
    /// by an explicit wake.
    pub claimed_at: f64,
}

impl CloudOperation {
    pub fn settled(&self) -> bool {
        settled(&self.status)
    }

    pub fn digest(
        cloud_agent_id: &str,
        thread_id: &str,
        source_kind: &str,
        source_uid: &str,
        epoch: i64,
        text: &str,
    ) -> String {
        digest(&[
            cloud_agent_id,
            thread_id,
            source_kind,
            source_uid,
            &epoch.to_string(),
            text,
        ])
    }

    pub fn to_json(&self) -> Value {
        json!({
            "operation_id": self.operation_id,
            "cloud_agent_id": self.cloud_agent_id,
            "source": {
                "kind": self.source_kind,
                "uid": self.source_uid,
                "registration_epoch": self.registration_epoch,
                "owner": self.owner.to_json(),
                "label": self.sender_label,
            },
            "recipient_uid": self.recipient_uid,
            "thread_id": self.thread_id,
            "conversation_id": self.conversation_id,
            "remote_session_id": self.remote_session_id,
            "status": self.status,
            "terminal": self.settled(),
            "claimed": self.claimed_at > 0.0,
            "remote_operation_id": self.remote_operation_id,
            "message_id": self.message_id,
            "reply": self.reply_text.as_ref().map(|text| json!({
                "text": text,
                "message_id": self.reply_message_id,
            })),
            "error": self.error_code.as_ref().map(|code| json!({
                "code": code,
                "message": self.error_message,
            })),
            "created": self.created,
            "updated": self.updated,
        })
    }
}

fn agent_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CloudAgent> {
    Ok(CloudAgent {
        id: row.get(0)?,
        alias: row.get(1)?,
        organization_id: row.get(2)?,
        org_slug: row.get(3)?,
        endpoint: row.get(4)?,
        remote_agent_id: row.get(5)?,
        runtime: row.get(6)?,
        model: row.get(7)?,
        capabilities: json_text(&row.get::<_, String>(8)?),
        provider_session_id: row.get(9)?,
        provenance: row
            .get::<_, Option<String>>(10)?
            .map(|text| json_text(&text))
            .unwrap_or(Value::Null),
        recipient_uid: row.get(11)?,
        consent_scope: row.get(12)?,
        state: row.get(13)?,
        created: row.get(14)?,
        updated: row.get(15)?,
    })
}

fn json_text(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

const AGENT_COLUMNS: &str = "id, alias, organization_id, org_slug, endpoint, \
remote_agent_id, runtime, model, capabilities, provider_session_id, provenance, \
recipient_uid, consent_scope, state, created, updated";

fn owner_of(issuer: String, subject: String) -> Owner {
    Owner { issuer, subject }
}

fn source_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CloudSource> {
    Ok(CloudSource {
        cloud_agent_id: row.get(0)?,
        kind: row.get(1)?,
        uid: row.get(2)?,
        registration_epoch: row.get(3)?,
        owner: owner_of(row.get(4)?, row.get(5)?),
        label: row.get(6)?,
        scope: row.get(7)?,
        granted_by: row.get(8)?,
        granted_at: row.get(9)?,
        invoker_id: row.get(10)?,
        state: row.get(11)?,
    })
}

const SOURCE_COLUMNS: &str = "cloud_agent_id, source_kind, source_uid, \
registration_epoch, owner_issuer, owner_subject, label, scope, granted_by, \
granted_at, invoker_id, state";

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CloudSession> {
    Ok(CloudSession {
        id: row.get(0)?,
        cloud_agent_id: row.get(1)?,
        source_kind: row.get(2)?,
        source_uid: row.get(3)?,
        registration_epoch: row.get(4)?,
        owner: owner_of(row.get(5)?, row.get(6)?),
        thread_id: row.get(7)?,
        conversation_id: row.get(8)?,
        remote_session_id: row.get(9)?,
        provider_session_id: row.get(10)?,
        invoker_id: row.get(11)?,
        created: row.get(12)?,
    })
}

const SESSION_COLUMNS: &str = "id, cloud_agent_id, source_kind, source_uid, \
registration_epoch, owner_issuer, owner_subject, thread_id, conversation_id, \
remote_session_id, provider_session_id, invoker_id, created";

fn operation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CloudOperation> {
    Ok(CloudOperation {
        operation_id: row.get(0)?,
        cloud_agent_id: row.get(1)?,
        source_kind: row.get(2)?,
        source_uid: row.get(3)?,
        registration_epoch: row.get(4)?,
        owner: owner_of(row.get(5)?, row.get(6)?),
        recipient_uid: row.get(7)?,
        sender_label: row.get(8)?,
        thread_id: row.get(9)?,
        conversation_id: row.get(10)?,
        remote_session_id: row.get(11)?,
        request_digest: row.get(12)?,
        request_text: row.get(13)?,
        status: row.get(14)?,
        remote_operation_id: row.get(15)?,
        message_id: row.get(16)?,
        reply_text: row.get(17)?,
        reply_message_id: row.get(18)?,
        error_code: row.get(19)?,
        error_message: row.get(20)?,
        created: row.get(21)?,
        updated: row.get(22)?,
        grant_admitted_at: row.get(23)?,
        claimed_at: row.get(24)?,
    })
}

const OPERATION_COLUMNS: &str = "operation_id, cloud_agent_id, source_kind, \
source_uid, registration_epoch, owner_issuer, owner_subject, recipient_uid, \
sender_label, thread_id, conversation_id, remote_session_id, request_digest, \
request_text, status, remote_operation_id, message_id, reply_text, \
reply_message_id, error_code, error_message, created, updated, grant_admitted_at, \
claimed_at";

fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_unique_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

impl Store {
    pub fn cloud_agent(&self, id: &str) -> Result<Option<CloudAgent>> {
        self.read_tx(|conn| {
            Ok(conn.query_opt(
                &format!("SELECT {AGENT_COLUMNS} FROM cloud_agents WHERE id=?1"),
                [id],
                agent_row,
            )?)
        })
    }

    pub fn cloud_agent_by_alias(&self, alias: &str) -> Result<Option<CloudAgent>> {
        self.read_tx(|conn| {
            Ok(conn.query_opt(
                &format!("SELECT {AGENT_COLUMNS} FROM cloud_agents WHERE alias=?1"),
                [alias],
                agent_row,
            )?)
        })
    }

    pub fn cloud_agents(&self) -> Result<Vec<CloudAgent>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                &format!("SELECT {AGENT_COLUMNS} FROM cloud_agents ORDER BY created, id"),
                [],
                agent_row,
            )?)
        })
    }

    /// Every cloud target an alias holds (one per remote tenant target).
    pub fn cloud_agents_for_alias(&self, alias: &str) -> Result<Vec<CloudAgent>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                &format!(
                    "SELECT {AGENT_COLUMNS} FROM cloud_agents WHERE alias=?1 ORDER BY created, id"
                ),
                [alias],
                agent_row,
            )?)
        })
    }

    /// Insert or refresh the exact same stable remote target. The alias is the
    /// canonical Threads key and is never repointed to another identity.
    pub fn cloud_agent_connect(&self, agent: &CloudAgent) -> Result<CloudAgent> {
        self.write_tx(|conn| {
            if let Some(existing) = conn.query_opt(
                &format!("SELECT {AGENT_COLUMNS} FROM cloud_agents WHERE alias=?1"),
                [agent.alias.as_str()],
                agent_row,
            )? {
                if existing.remote_agent_id != agent.remote_agent_id
                    || existing.organization_id != agent.organization_id
                    || existing.endpoint != agent.endpoint
                {
                    return Err(Error::gate_coded(
                        "cloud_agent_alias_taken",
                        "cloud agent alias is already bound to a different remote identity",
                    ));
                }
                // The alias row this connection was created against is the
                // only canonical recipient its requests may bind. A
                // re-registered alias is a new local identity: reusing the
                // old connection would attribute new paid turns to it.
                if !existing.recipient_uid.is_empty()
                    && existing.recipient_uid != agent.recipient_uid
                {
                    return Err(Error::gate_coded(
                        "cloud_agent_recipient_changed",
                        "the local agent alias was re-registered since this cloud \
                         connection was created; disconnect and connect it again \
                         explicitly — the old identity is retained for history",
                    ));
                }
                conn.execute(
                    "UPDATE cloud_agents SET runtime=?2, model=?3, capabilities=?4, \
                     provider_session_id=?5, provenance=?6, recipient_uid=?7, \
                     consent_scope=?8, state=?9, updated=?10 WHERE id=?1",
                    params![
                        existing.id,
                        agent.runtime,
                        agent.model,
                        agent.capabilities.to_string(),
                        agent.provider_session_id,
                        agent.provenance.to_string(),
                        agent.recipient_uid,
                        agent.consent_scope,
                        STATE_CONNECTED,
                        now(),
                    ],
                )?;
            } else if conn
                .query_opt(
                    &format!(
                        "SELECT {AGENT_COLUMNS} FROM cloud_agents \
                         WHERE endpoint=?1 AND organization_id=?2 AND remote_agent_id=?3"
                    ),
                    params![agent.endpoint, agent.organization_id, agent.remote_agent_id],
                    agent_row,
                )?
                .is_some()
            {
                return Err(Error::gate_coded(
                    "cloud_agent_identity_conflict",
                    "cloud agent identity is already connected under another local alias",
                ));
            } else {
                conn.execute(
                    "INSERT INTO cloud_agents(id, alias, organization_id, org_slug, \
                     endpoint, remote_agent_id, runtime, model, capabilities, \
                     provider_session_id, provenance, recipient_uid, consent_scope, \
                     state, created, updated) \
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                    params![
                        agent.id,
                        agent.alias,
                        agent.organization_id,
                        agent.org_slug,
                        agent.endpoint,
                        agent.remote_agent_id,
                        agent.runtime,
                        agent.model,
                        agent.capabilities.to_string(),
                        agent.provider_session_id,
                        agent.provenance.to_string(),
                        agent.recipient_uid,
                        agent.consent_scope,
                        STATE_CONNECTED,
                        agent.created,
                        agent.updated,
                    ],
                )?;
            }
            conn.query_opt(
                &format!("SELECT {AGENT_COLUMNS} FROM cloud_agents WHERE alias=?1"),
                [agent.alias.as_str()],
                agent_row,
            )?
            .ok_or_else(|| Error::internal("cloud agent write did not persist"))
        })
    }

    pub fn cloud_agent_set_state(&self, id: &str, state: &str) -> Result<CloudAgent> {
        self.write_tx(|conn| {
            conn.execute(
                "UPDATE cloud_agents SET state=?2, updated=?3 WHERE id=?1",
                params![id, state, now()],
            )?;
            conn.query_opt(
                &format!("SELECT {AGENT_COLUMNS} FROM cloud_agents WHERE id=?1"),
                [id],
                agent_row,
            )?
            .ok_or_else(|| {
                Error::gate_coded("cloud_agent_not_connected", "cloud agent is unavailable")
            })
        })
    }

    /// Grant (or refresh) one invoker source on this target. The operator's
    /// own grant is created at connect; a peer's requires its own enrollment.
    pub fn cloud_agent_source_grant(&self, source: &CloudSource) -> Result<()> {
        self.write_tx(|conn| {
            // A grant refresh is consent, never identity: the first verified
            // `invoker_id` is this source's immutable pin. A supplied
            // different value is refused BEFORE any row is written, so a
            // refused reconnect leaves the retained pin exactly as it was
            // instead of adopting a new owner's context or half-refreshing.
            if let Some(supplied) = source.invoker_id.as_deref() {
                let existing: Option<Option<String>> = conn.query_opt(
                    "SELECT invoker_id FROM cloud_agent_sources \
                     WHERE cloud_agent_id=?1 AND source_kind=?2 AND source_uid=?3 \
                     AND registration_epoch=?4 AND owner_issuer=?5 AND owner_subject=?6",
                    params![
                        source.cloud_agent_id,
                        source.kind,
                        source.uid,
                        source.registration_epoch,
                        source.owner.issuer,
                        source.owner.subject,
                    ],
                    |row| row.get(0),
                )?;
                if let Some(Some(pinned)) = existing {
                    if pinned != supplied {
                        return Err(Error::gate_coded(
                            "cloud_agent_invoker_changed",
                            "the contract reported a different stable owner (invoker_id) than \
                             this source is pinned to; its consent and provider session are \
                             retained unchanged",
                        ));
                    }
                }
            }
            // A peer re-enrollment under a different human REPLACES its
            // grant: the same native uid+epoch is not allowed two active
            // owners, or a cached session/reply could be inherited. The old
            // owner's sessions and operations stay keyed to that owner and
            // become unreachable (the grant is revoked, not deleted).
            if source.kind == SOURCE_PEER {
                conn.execute(
                    "UPDATE cloud_agent_sources SET state=?7 \
                     WHERE cloud_agent_id=?1 AND source_kind=?2 AND source_uid=?3 \
                     AND registration_epoch=?4 \
                     AND NOT (owner_issuer=?5 AND owner_subject=?6)",
                    params![
                        source.cloud_agent_id,
                        source.kind,
                        source.uid,
                        source.registration_epoch,
                        source.owner.issuer,
                        source.owner.subject,
                        SOURCE_REVOKED,
                    ],
                )?;
            }
            conn.execute(
                "INSERT INTO cloud_agent_sources(cloud_agent_id, source_kind, source_uid, \
                 registration_epoch, owner_issuer, owner_subject, label, scope, granted_by, \
                 granted_at, invoker_id, state) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) \
                 ON CONFLICT(cloud_agent_id, source_kind, source_uid, registration_epoch, \
                             owner_issuer, owner_subject) \
                 DO UPDATE SET label=excluded.label, scope=excluded.scope, \
                 granted_by=excluded.granted_by, granted_at=excluded.granted_at, \
                 invoker_id=COALESCE(excluded.invoker_id, cloud_agent_sources.invoker_id), \
                 state=excluded.state",
                params![
                    source.cloud_agent_id,
                    source.kind,
                    source.uid,
                    source.registration_epoch,
                    source.owner.issuer,
                    source.owner.subject,
                    source.label,
                    source.scope,
                    source.granted_by,
                    source.granted_at,
                    source.invoker_id,
                    source.state,
                ],
            )?;
            Ok(())
        })
    }

    pub fn cloud_agent_source(
        &self,
        cloud_agent_id: &str,
        kind: &str,
        uid: &str,
        epoch: i64,
        owner: &Owner,
    ) -> Result<Option<CloudSource>> {
        self.read_tx(|conn| {
            Ok(conn.query_opt(
                &format!(
                    "SELECT {SOURCE_COLUMNS} FROM cloud_agent_sources \
                     WHERE cloud_agent_id=?1 AND source_kind=?2 AND source_uid=?3 \
                     AND registration_epoch=?4 AND owner_issuer=?5 AND owner_subject=?6"
                ),
                params![
                    cloud_agent_id,
                    kind,
                    uid,
                    epoch,
                    owner.issuer,
                    owner.subject
                ],
                source_row,
            )?)
        })
    }

    pub fn cloud_agent_sources(&self, cloud_agent_id: &str) -> Result<Vec<CloudSource>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                &format!(
                    "SELECT {SOURCE_COLUMNS} FROM cloud_agent_sources \
                     WHERE cloud_agent_id=?1 ORDER BY source_kind, source_uid, \
                     registration_epoch, owner_issuer, owner_subject"
                ),
                [cloud_agent_id],
                source_row,
            )?)
        })
    }

    /// Pin the contract's stable `invoker_id` on one source. The first
    /// verified value wins; a different one for the same logical owner is
    /// refused, so a rotated credential or another owner can never silently
    /// reuse this source's provider context.
    pub fn cloud_agent_source_pin_invoker(
        &self,
        cloud_agent_id: &str,
        kind: &str,
        uid: &str,
        epoch: i64,
        owner: &Owner,
        invoker_id: &str,
    ) -> Result<()> {
        self.write_tx(|conn| {
            let existing = conn.query_opt(
                &format!(
                    "SELECT {SOURCE_COLUMNS} FROM cloud_agent_sources \
                     WHERE cloud_agent_id=?1 AND source_kind=?2 AND source_uid=?3 \
                     AND registration_epoch=?4 AND owner_issuer=?5 AND owner_subject=?6"
                ),
                params![
                    cloud_agent_id,
                    kind,
                    uid,
                    epoch,
                    owner.issuer,
                    owner.subject
                ],
                source_row,
            )?;
            let Some(existing) = existing else {
                return Err(Error::gate_coded(
                    "cloud_agent_consent_required",
                    "this invoker source has no explicit cloud-agent consent on this target",
                ));
            };
            match existing.invoker_id.as_deref() {
                Some(pinned) if pinned == invoker_id => Ok(()),
                Some(_) => Err(Error::gate_coded(
                    "cloud_agent_invoker_changed",
                    "the contract reported a different stable owner (invoker_id) than this \
                     source is pinned to; no reply was recorded",
                )),
                None => {
                    conn.execute(
                        "UPDATE cloud_agent_sources SET invoker_id=?7 \
                         WHERE cloud_agent_id=?1 AND source_kind=?2 AND source_uid=?3 \
                         AND registration_epoch=?4 AND owner_issuer=?5 AND owner_subject=?6 \
                         AND invoker_id IS NULL",
                        params![
                            cloud_agent_id,
                            kind,
                            uid,
                            epoch,
                            owner.issuer,
                            owner.subject,
                            invoker_id
                        ],
                    )?;
                    Ok(())
                }
            }
        })
    }

    /// Revoke one exact source. A different epoch/UID/owner is a different
    /// source and never inherits this grant.
    pub fn cloud_agent_source_revoke(
        &self,
        cloud_agent_id: &str,
        kind: &str,
        uid: &str,
        epoch: i64,
        owner: &Owner,
    ) -> Result<()> {
        self.write_tx(|conn| {
            conn.execute(
                "UPDATE cloud_agent_sources SET state=?7 \
                 WHERE cloud_agent_id=?1 AND source_kind=?2 AND source_uid=?3 \
                 AND registration_epoch=?4 AND owner_issuer=?5 AND owner_subject=?6",
                params![
                    cloud_agent_id,
                    kind,
                    uid,
                    epoch,
                    owner.issuer,
                    owner.subject,
                    SOURCE_REVOKED
                ],
            )?;
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn cloud_agent_session(
        &self,
        cloud_agent_id: &str,
        kind: &str,
        uid: &str,
        epoch: i64,
        owner: &Owner,
        thread_id: &str,
    ) -> Result<Option<CloudSession>> {
        self.read_tx(|conn| {
            Ok(conn.query_opt(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM cloud_agent_sessions \
                     WHERE cloud_agent_id=?1 AND source_kind=?2 AND source_uid=?3 \
                     AND registration_epoch=?4 AND owner_issuer=?5 AND owner_subject=?6 \
                     AND thread_id=?7"
                ),
                params![
                    cloud_agent_id,
                    kind,
                    uid,
                    epoch,
                    owner.issuer,
                    owner.subject,
                    thread_id
                ],
                session_row,
            )?)
        })
    }

    pub fn cloud_agent_sessions(&self, cloud_agent_id: &str) -> Result<Vec<CloudSession>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM cloud_agent_sessions \
                     WHERE cloud_agent_id=?1 ORDER BY created, id"
                ),
                [cloud_agent_id],
                session_row,
            )?)
        })
    }

    /// Bind one thread under one source to a server-issued session. Two
    /// sources on the same canonical thread hold distinct sessions; a
    /// different source (including a different owner) can never overwrite an
    /// existing binding.
    #[allow(clippy::too_many_arguments)]
    pub fn cloud_agent_bind_session(
        &self,
        cloud_agent_id: &str,
        kind: &str,
        uid: &str,
        epoch: i64,
        owner: &Owner,
        thread_id: &str,
        conversation_id: &str,
        remote_session_id: &str,
        provider_session_id: Option<&str>,
        invoker_id: Option<&str>,
    ) -> Result<CloudSession> {
        if let Some(invoker_id) = invoker_id {
            self.cloud_agent_source_pin_invoker(
                cloud_agent_id,
                kind,
                uid,
                epoch,
                owner,
                invoker_id,
            )?;
        }
        self.write_tx(|conn| {
            if let Some(existing) = conn.query_opt(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM cloud_agent_sessions \
                     WHERE cloud_agent_id=?1 AND source_kind=?2 AND source_uid=?3 \
                     AND registration_epoch=?4 AND owner_issuer=?5 AND owner_subject=?6 \
                     AND thread_id=?7"
                ),
                params![
                    cloud_agent_id,
                    kind,
                    uid,
                    epoch,
                    owner.issuer,
                    owner.subject,
                    thread_id
                ],
                session_row,
            )? {
                if existing.conversation_id != conversation_id {
                    return Err(Error::gate_coded(
                        "cloud_agent_session_mismatch",
                        "this source's work conversation is bound to a different opaque \
                         conversation key",
                    ));
                }
                if existing.remote_session_id != remote_session_id {
                    // An ordinary access-credential renewal is not consent to
                    // a new empty provider context. The original binding is
                    // retained and the unexpected rotation is refused; a
                    // genuinely new context needs an explicit new connection.
                    return Err(Error::gate_coded(
                        "cloud_agent_session_mismatch",
                        "the contract derived a different provider session for this source \
                         and work conversation; the original binding is retained — open an \
                         explicit new connection/context instead of replacing it",
                    ));
                }
                return Ok(existing);
            }
            let at = now();
            let id = format!(
                "clds-{}",
                &digest(&[
                    cloud_agent_id,
                    kind,
                    uid,
                    &epoch.to_string(),
                    &owner.issuer,
                    &owner.subject,
                    thread_id
                ])[..24]
            );
            conn.execute(
                "INSERT INTO cloud_agent_sessions(id, cloud_agent_id, source_kind, source_uid, \
                 registration_epoch, owner_issuer, owner_subject, thread_id, conversation_id, \
                 remote_session_id, provider_session_id, invoker_id, created) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    id,
                    cloud_agent_id,
                    kind,
                    uid,
                    epoch,
                    owner.issuer,
                    owner.subject,
                    thread_id,
                    conversation_id,
                    remote_session_id,
                    provider_session_id,
                    invoker_id,
                    at
                ],
            )
            .map_err(|error| {
                if is_unique_violation(&error) {
                    Error::gate_coded(
                        "cloud_agent_session_mismatch",
                        "remote session or conversation is already bound under another source",
                    )
                } else {
                    error.into()
                }
            })?;
            Ok(CloudSession {
                id,
                cloud_agent_id: cloud_agent_id.to_string(),
                source_kind: kind.to_string(),
                source_uid: uid.to_string(),
                registration_epoch: epoch,
                owner: owner.clone(),
                thread_id: thread_id.to_string(),
                conversation_id: conversation_id.to_string(),
                remote_session_id: remote_session_id.to_string(),
                provider_session_id: provider_session_id.map(str::to_string),
                invoker_id: invoker_id.map(str::to_string),
                created: at,
            })
        })
    }

    /// Persist the local request before any transport happens. A duplicate
    /// operation id with the same digest returns the stored row; a different
    /// payload is a conflict. Concurrent callers serialize on the write tx.
    pub fn cloud_agent_operation_begin(
        &self,
        operation: &CloudOperation,
    ) -> Result<(CloudOperation, bool)> {
        self.write_tx(|conn| {
            if let Some(existing) = conn.query_opt(
                &format!(
                    "SELECT {OPERATION_COLUMNS} FROM cloud_agent_operations \
                     WHERE cloud_agent_id=?1 AND operation_id=?2"
                ),
                params![operation.cloud_agent_id, operation.operation_id],
                operation_row,
            )? {
                if existing.request_digest != operation.request_digest {
                    return Err(Error::conflict_coded(
                        "cloud_agent_replay_conflict",
                        "operation id was already used with a different request; nothing was sent",
                    ));
                }
                return Ok((existing, false));
            }
            // One unsettled turn per provider session: refuse BEFORE the new
            // claim row exists, so a no-wake A and a later B cannot retain
            // mutually blocking claims.
            if let Some(other) = Self::cloud_agent_unsettled_in(
                &conn,
                &operation.cloud_agent_id,
                &operation.source_kind,
                &operation.source_uid,
                operation.registration_epoch,
                &operation.owner,
                &operation.conversation_id,
                &operation.operation_id,
            )? {
                return Err(Error::conflict_coded(
                    "cloud_agent_session_busy",
                    format!(
                        "this work conversation already has an unsettled turn ({other}); \
                         read it back before sending another — nothing was sent"
                    ),
                ));
            }
            // The admission lineage: the consent grant this request is
            // admitted under, identified by its immutable `granted_at`. A
            // later wake must present the SAME current grant; a revoked and
            // regranted consent is a different lineage. No grant row (a
            // direct caller that never proved consent) records 0, which no
            // real grant timestamp can equal, so a wake is refused.
            let admitted_at: f64 = conn
                .query_opt(
                    "SELECT granted_at FROM cloud_agent_sources \
                     WHERE cloud_agent_id=?1 AND source_kind=?2 AND source_uid=?3 \
                     AND registration_epoch=?4 AND owner_issuer=?5 AND owner_subject=?6",
                    params![
                        operation.cloud_agent_id,
                        operation.source_kind,
                        operation.source_uid,
                        operation.registration_epoch,
                        operation.owner.issuer,
                        operation.owner.subject,
                    ],
                    |row| row.get(0),
                )?
                .unwrap_or(0.0);
            conn.execute(
                "INSERT INTO cloud_agent_operations(operation_id, cloud_agent_id, source_kind, \
                 source_uid, registration_epoch, owner_issuer, owner_subject, recipient_uid, \
                 sender_label, thread_id, conversation_id, remote_session_id, request_digest, \
                 request_text, status, created, updated, grant_admitted_at) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
                params![
                    operation.operation_id,
                    operation.cloud_agent_id,
                    operation.source_kind,
                    operation.source_uid,
                    operation.registration_epoch,
                    operation.owner.issuer,
                    operation.owner.subject,
                    operation.recipient_uid,
                    operation.sender_label,
                    operation.thread_id,
                    operation.conversation_id,
                    operation.remote_session_id,
                    operation.request_digest,
                    operation.request_text,
                    STATUS_QUEUED,
                    operation.created,
                    operation.updated,
                    admitted_at,
                ],
            )?;
            Ok((
                CloudOperation {
                    status: STATUS_QUEUED.to_string(),
                    grant_admitted_at: admitted_at,
                    ..operation.clone()
                },
                true,
            ))
        })
    }

    pub fn cloud_agent_operation(
        &self,
        cloud_agent_id: &str,
        operation_id: &str,
    ) -> Result<Option<CloudOperation>> {
        self.read_tx(|conn| self.cloud_agent_operation_in(conn, cloud_agent_id, operation_id))
    }

    /// Read one operation witness inside an open transaction. The cloud
    /// completion path uses it to CAS the receipt in the same transaction as
    /// the canonical result.
    pub(crate) fn cloud_agent_operation_in(
        &self,
        tx: &impl super::StoreConn,
        cloud_agent_id: &str,
        operation_id: &str,
    ) -> Result<Option<CloudOperation>> {
        Ok(tx.query_opt(
            &format!(
                "SELECT {OPERATION_COLUMNS} FROM cloud_agent_operations \
                 WHERE cloud_agent_id=?1 AND operation_id=?2"
            ),
            params![cloud_agent_id, operation_id],
            operation_row,
        )?)
    }

    /// Attach the canonical message and its immutable recipient to the
    /// operation inside the enqueue transaction. Idempotent for the same
    /// values; a different message or recipient is a conflict.
    pub(crate) fn cloud_agent_attach_in(
        &self,
        tx: &impl super::StoreConn,
        cloud_agent_id: &str,
        operation_id: &str,
        message_id: &str,
        recipient_uid: &str,
    ) -> Result<()> {
        let changed = tx.execute(
            "UPDATE cloud_agent_operations SET message_id=?3, recipient_uid=?4, updated=?5 \
             WHERE cloud_agent_id=?1 AND operation_id=?2 \
             AND (message_id IS NULL OR message_id=?3) \
             AND (recipient_uid='' OR recipient_uid=?4)",
            params![
                cloud_agent_id,
                operation_id,
                message_id,
                recipient_uid,
                now()
            ],
        )?;
        if changed == 0 {
            return Err(Error::conflict_coded(
                "cloud_agent_message_conflict",
                "operation is already bound to a different canonical message or recipient",
            ));
        }
        Ok(())
    }

    pub fn cloud_agent_operations(
        &self,
        cloud_agent_id: &str,
        thread_id: Option<&str>,
    ) -> Result<Vec<CloudOperation>> {
        self.read_tx(|conn| match thread_id {
            Some(thread) => Ok(conn.query_vec(
                &format!(
                    "SELECT {OPERATION_COLUMNS} FROM cloud_agent_operations \
                     WHERE cloud_agent_id=?1 AND thread_id=?2 ORDER BY created, operation_id"
                ),
                params![cloud_agent_id, thread],
                operation_row,
            )?),
            None => Ok(conn.query_vec(
                &format!(
                    "SELECT {OPERATION_COLUMNS} FROM cloud_agent_operations \
                     WHERE cloud_agent_id=?1 ORDER BY created, operation_id"
                ),
                [cloud_agent_id],
                operation_row,
            )?),
        })
    }

    /// Non-terminal operations whose submit may have reached the contract.
    /// A queue-only row that never crossed the transport boundary
    /// (`claimed_at = 0`, no remote id) is not in this worklist: only an
    /// explicit wake may submit it. A CLAIMED row that the contract accepted
    /// as queued IS here even without a remote id, because its outcome is
    /// real and only the platform can settle it.
    pub fn cloud_agent_pending_operations(
        &self,
        cloud_agent_id: &str,
    ) -> Result<Vec<CloudOperation>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                &format!(
                    "SELECT {OPERATION_COLUMNS} FROM cloud_agent_operations \
                     WHERE cloud_agent_id=?1 AND (\
                       status IN ('running','parked','unknown') \
                       OR (status='queued' AND (remote_operation_id IS NOT NULL \
                         OR claimed_at > 0))) \
                     ORDER BY created, operation_id"
                ),
                [cloud_agent_id],
                operation_row,
            )?)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn cloud_agent_operation_set(
        &self,
        cloud_agent_id: &str,
        operation_id: &str,
        status: &str,
        remote_operation_id: Option<&str>,
        reply_text: Option<&str>,
        reply_message_id: Option<&str>,
        error: Option<(&str, &str)>,
    ) -> Result<CloudOperation> {
        self.write_tx(|conn| {
            // A stale queued/running/unknown readback must never reopen a
            // settled receipt (that would hide a real completion). The CAS
            // misses on a terminal row and the current settled row is
            // returned instead.
            conn.execute(
                "UPDATE cloud_agent_operations SET status=?3, \
                 remote_operation_id=COALESCE(?4, remote_operation_id), \
                 reply_text=COALESCE(?5, reply_text), \
                 reply_message_id=COALESCE(?6, reply_message_id), \
                 error_code=?7, error_message=?8, updated=?9 \
                 WHERE cloud_agent_id=?1 AND operation_id=?2 \
                 AND status NOT IN ('completed','failed')",
                params![
                    cloud_agent_id,
                    operation_id,
                    status,
                    remote_operation_id,
                    reply_text,
                    reply_message_id,
                    error.map(|e| e.0),
                    error.map(|e| e.1),
                    now(),
                ],
            )?;
            conn.query_opt(
                &format!(
                    "SELECT {OPERATION_COLUMNS} FROM cloud_agent_operations \
                     WHERE cloud_agent_id=?1 AND operation_id=?2"
                ),
                params![cloud_agent_id, operation_id],
                operation_row,
            )?
            .ok_or_else(|| {
                Error::gate_coded(
                    "cloud_agent_operation_unknown",
                    "cloud agent operation is unavailable",
                )
            })
        })
    }

    /// Claim one queued operation for submission: a compare-and-set from
    /// `queued` to `running` that exactly one caller can win. Two identical
    /// concurrent sends (or a new-row/enqueue race) therefore submit once;
    /// the loser never calls the transport and is told the operation is
    /// already claimed instead of being handed the cached row.
    ///
    /// Eligibility is durable, not just the current status: `claimed_at = 0`
    /// is part of the predicate, so an operation that ever crossed (or tried
    /// to cross) the submission boundary can never be claimed again, even
    /// when the contract's accepted-queued answer — which carries no remote
    /// operation id — leaves the row `queued`. A row that never reached the
    /// transport keeps `claimed_at = 0` and stays retryable by an explicit
    /// wake, which is exactly the pre-transport refusal semantics. Restart
    /// recovery is unaffected: the claim itself sets `running`, and the
    /// readback worklist selects `running`/`parked`/`unknown` plus a
    /// `queued` row that already carries a remote id or a claim.
    pub fn cloud_agent_operation_claim(
        &self,
        cloud_agent_id: &str,
        operation_id: &str,
    ) -> Result<bool> {
        self.write_tx(|conn| {
            let at = now();
            let changed = conn.execute(
                "UPDATE cloud_agent_operations SET status=?3, claimed_at=?5, updated=?4 \
                 WHERE cloud_agent_id=?1 AND operation_id=?2 AND status='queued' \
                 AND claimed_at=0",
                params![cloud_agent_id, operation_id, STATUS_RUNNING, at, at],
            )?;
            Ok(changed == 1)
        })
    }

    /// Reconcile ONE settled operation's cached answer with the platform's
    /// own terminal readback. The caller has already proven read authority
    /// and matched the outcome, so this only records the readback's answer on
    /// the receipt: the settled status and the canonical Thread history are
    /// not rewritten. A read served afterwards therefore returns the
    /// platform's reply, not a stale cache copy.
    pub fn cloud_agent_operation_reconcile(
        &self,
        cloud_agent_id: &str,
        operation_id: &str,
        remote_operation_id: Option<&str>,
        reply_text: Option<&str>,
        error: Option<(&str, &str)>,
    ) -> Result<CloudOperation> {
        self.write_tx(|conn| {
            conn.execute(
                "UPDATE cloud_agent_operations SET \
                 remote_operation_id=COALESCE(?3, remote_operation_id), \
                 reply_text=COALESCE(?4, reply_text), \
                 error_code=?5, error_message=?6, updated=?7 \
                 WHERE cloud_agent_id=?1 AND operation_id=?2 \
                 AND status IN ('completed','failed')",
                params![
                    cloud_agent_id,
                    operation_id,
                    remote_operation_id,
                    reply_text,
                    error.map(|e| e.0),
                    error.map(|e| e.1),
                    now(),
                ],
            )?;
            conn.query_opt(
                &format!(
                    "SELECT {OPERATION_COLUMNS} FROM cloud_agent_operations \
                     WHERE cloud_agent_id=?1 AND operation_id=?2"
                ),
                params![cloud_agent_id, operation_id],
                operation_row,
            )?
            .ok_or_else(|| {
                Error::gate_coded(
                    "cloud_agent_operation_unknown",
                    "cloud agent operation is unavailable",
                )
            })
        })
    }

    /// The contract allows one unsettled turn per provider session. Returns
    /// another operation id already accepted/queued/running/parked for the
    /// same source and conversation, so a second turn is refused locally
    /// without a transport call.
    /// Remove a claim that never acquired a canonical message (a refused
    /// canonical enqueue). Terminal/reply-bearing rows are never discarded.
    pub fn cloud_agent_operation_discard(
        &self,
        cloud_agent_id: &str,
        operation_id: &str,
    ) -> Result<()> {
        self.write_tx(|conn| {
            conn.execute(
                "DELETE FROM cloud_agent_operations \
                 WHERE cloud_agent_id=?1 AND operation_id=?2 \
                 AND message_id IS NULL AND reply_text IS NULL \
                 AND status NOT IN ('completed','failed')",
                params![cloud_agent_id, operation_id],
            )?;
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn cloud_agent_unsettled_in(
        tx: &impl super::StoreConn,
        cloud_agent_id: &str,
        kind: &str,
        uid: &str,
        epoch: i64,
        owner: &Owner,
        conversation_id: &str,
        except_operation: &str,
    ) -> Result<Option<String>> {
        Ok(tx.query_opt(
            "SELECT operation_id FROM cloud_agent_operations \
             WHERE cloud_agent_id=?1 AND source_kind=?2 AND source_uid=?3 \
             AND registration_epoch=?4 AND owner_issuer=?5 AND owner_subject=?6 \
             AND conversation_id=?7 AND operation_id<>?8 AND reply_text IS NULL \
             AND status IN ('queued','running','parked','unknown') LIMIT 1",
            params![
                cloud_agent_id,
                kind,
                uid,
                epoch,
                owner.issuer,
                owner.subject,
                conversation_id,
                except_operation
            ],
            |row| row.get::<_, String>(0),
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn cloud_agent_unsettled(
        &self,
        cloud_agent_id: &str,
        kind: &str,
        uid: &str,
        epoch: i64,
        owner: &Owner,
        conversation_id: &str,
        except_operation: &str,
    ) -> Result<Option<String>> {
        self.read_tx(|conn| {
            Self::cloud_agent_unsettled_in(
                conn,
                cloud_agent_id,
                kind,
                uid,
                epoch,
                owner,
                conversation_id,
                except_operation,
            )
        })
    }

    /// Whether this alias still carries cloud-agent private content: a
    /// request or result row the cloud transport wrote. A restored workspace
    /// can hold such rows with no connection authority at all (the four
    /// cloud tables are outside the coordination profile), so a projection
    /// must fail closed instead of treating the alias as an ordinary local
    /// agent.
    pub fn alias_has_cloud_content(&self, alias: &str) -> Result<bool> {
        self.read_tx(|conn| {
            let present: i64 = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE alias=?1 AND source='cloud_agent')",
                [alias],
                |row| row.get(0),
            )?;
            Ok(present != 0)
        })
    }

    /// The alias' rows whose IMMUTABLE origin provenance reaches a cloud
    /// request/result — a routed `worker_result`/`worker_notice`, and anything
    /// derived from that in turn. They are cloud content off the cloud alias
    /// too, so a projection must bind them to the UIDs the delivery carried
    /// instead of treating them as ordinary local mail.
    ///
    /// `messages.origin` is written by the daemon's outbox and never by a
    /// request, so following it is not a caller-controlled claim. Rows with no
    /// origin are ordinary.
    pub fn cloud_derived_messages(&self, alias: &str) -> Result<Vec<CloudDerivedRow>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                "WITH RECURSIVE cloud(id) AS ( \
                   SELECT id FROM messages \
                    WHERE source='cloud_agent' \
                       OR alias IN (SELECT alias FROM agents WHERE provider=?2) \
                   UNION \
                   SELECT m.id FROM messages m JOIN cloud c ON m.origin=c.id \
                 ) \
                 SELECT id, sender_uid, recipient_uid FROM messages \
                  WHERE alias=?1 AND id IN (SELECT id FROM cloud)",
                params![alias, crate::cloud_agent::PROVIDER],
                |row| {
                    Ok(CloudDerivedRow {
                        message_id: row.get(0)?,
                        sender_uid: row.get(1)?,
                        recipient_uid: row.get(2)?,
                    })
                },
            )?)
        })
    }

    /// The alias' message ids whose immutable provenance does NOT reach a
    /// cloud request/result — the ordinary local mail of an ordinary alias.
    ///
    /// This is the complement of [`Self::cloud_derived_messages`] over the
    /// same alias, so a projection can admit an ordinary alias' own mail
    /// EXPLICITLY (an enumerated snapshot) instead of assuming that any row
    /// the derived map does not name is ordinary. It is not: a routed cloud
    /// result that resolves after the snapshot is absent from both, and
    /// assuming it ordinary would hand another source's reply to whoever
    /// asks. Only rows enumerated here render.
    /// Every id this alias' own rows can carry, so a projection can admit
    /// them explicitly instead of by absence. Messages are the obvious half;
    /// thread entries are the other, because an entry's `message_id` need not
    /// name a message row (artifact cards and hire proposals never do), and a
    /// projection that enumerated only messages would withhold them.
    pub fn ordinary_message_ids(&self, alias: &str) -> Result<Vec<String>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                "WITH RECURSIVE cloud(id) AS ( \
                   SELECT id FROM messages \
                    WHERE source='cloud_agent' \
                       OR alias IN (SELECT alias FROM agents WHERE provider=?2) \
                   UNION \
                   SELECT m.id FROM messages m JOIN cloud c ON m.origin=c.id \
                 ) \
                 SELECT id FROM messages \
                  WHERE alias=?1 AND id NOT IN (SELECT id FROM cloud) \
                 UNION \
                 SELECT e.message_id FROM thread_entries e \
                   JOIN threads t ON t.id = e.thread_id \
                  WHERE t.alias = ?1 AND e.message_id IS NOT NULL \
                    AND e.message_id NOT IN (SELECT id FROM cloud)",
                params![alias, crate::cloud_agent::PROVIDER],
                |row| row.get::<_, String>(0),
            )?)
        })
    }

    pub fn cloud_agents_with_pending_operations(&self) -> Result<Vec<String>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                "SELECT DISTINCT cloud_agent_id FROM cloud_agent_operations \
                 WHERE status IN ('running','parked','unknown') \
                    OR (status='queued' AND (remote_operation_id IS NOT NULL \
                      OR claimed_at > 0)) \
                 ORDER BY cloud_agent_id",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
    }
}
