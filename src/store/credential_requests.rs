//! CAD-1085: metadata-only credential requests and their one-use human
//! entry attempts.
//!
//! A request is the durable *intent* to let a human enroll, rotate or
//! verify one Connection credential through the trusted entry page. It
//! holds identifiers, the selected protected context (workspace, custody
//! domain/epoch, qualified-runtime digest), the opaque target account for a
//! fresh enrollment and fixed status words — never a credential, a
//! fragment, a low-entropy digest, a username or transport setting. Those
//! last values live only in the isolated form and cross exactly one submit
//! call into the daemon's provider mutation, which screens them with the
//! secret before anything is persisted (r3 I2).
//!
//! A form nonce is a 256-bit random value the entry page receives once.
//! Only its SHA-256 digest is persisted, bound to the live authenticated
//! owner session, the request revision, provider/verb/Connection, the
//! expected credential/authorization revisions and the current protected
//! context digest. It lives for [`NONCE_TTL_MS`] and is claimed at most
//! once — atomically, before the daemon forwards anything — so a replayed
//! submit, a second tab or a lost acknowledgement can never execute the
//! mutation twice.
//!
//! The tables are additive (`IF NOT EXISTS`, one forward migration) and do
//! not touch `platform_credentials`, `connection_metadata` or the immutable
//! ciphertext rows.

use super::{Store, StoreConn};
use crate::error::{Error, Result};
use rusqlite::params;
use serde_json::{json, Value};

/// The additive schema for native credential requests. The owning daemon
/// migration executes this batch once, folded together with the connection
/// dispatch schema; every statement is idempotent so a half-applied forward
/// migration converges on reopen.
pub(crate) const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS credential_requests(
 request_id TEXT PRIMARY KEY,
 request_revision INTEGER NOT NULL CHECK(request_revision>0),
 provider TEXT NOT NULL CHECK(length(provider) BETWEEN 1 AND 128),
 verb TEXT NOT NULL CHECK(verb IN ('enroll','rotate','verify')),
 connection_id TEXT,
 target_id TEXT NOT NULL CHECK(length(target_id) BETWEEN 1 AND 128),
 enrollment_shape TEXT CHECK(enrollment_shape IS NULL OR enrollment_shape IN ('token','smtp','native-model-api-key','native-model-devin-credentials')),
 scopes TEXT NOT NULL DEFAULT '[]',
 expected_credential_revision INTEGER CHECK(expected_credential_revision IS NULL OR expected_credential_revision>0),
 expected_authorization_revision INTEGER CHECK(expected_authorization_revision IS NULL OR expected_authorization_revision>0),
 custody_domain TEXT NOT NULL CHECK(length(custody_domain) BETWEEN 1 AND 64),
 custody_epoch INTEGER NOT NULL CHECK(custody_epoch>0),
 context_digest TEXT NOT NULL CHECK(length(context_digest) BETWEEN 16 AND 128),
 requester_kind TEXT NOT NULL CHECK(requester_kind IN ('operator_cli','operator_ui','agent')),
 requester_participant TEXT,
 conversation_id TEXT,
 target_agent_uid TEXT,
 state TEXT NOT NULL CHECK(state IN ('pending','submitting','done','cancelled','expired','unknown')),
 status TEXT NOT NULL,
 result_code TEXT,
 result_connection_id TEXT,
 verification_request_id TEXT,
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL,
 expires_at_ms INTEGER NOT NULL,
 CHECK(
  (verb='enroll' AND connection_id IS NULL) OR
  (verb IN ('rotate','verify') AND (connection_id IS NOT NULL OR
    enrollment_shape IN ('native-model-api-key','native-model-devin-credentials')))
 ));
CREATE UNIQUE INDEX IF NOT EXISTS credential_requests_verify
 ON credential_requests(verification_request_id) WHERE verification_request_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS credential_requests_conversation
 ON credential_requests(conversation_id, created_at_ms);
CREATE INDEX IF NOT EXISTS credential_requests_agent
 ON credential_requests(target_agent_uid, created_at_ms);
CREATE TABLE IF NOT EXISTS credential_request_nonces(
 nonce_digest TEXT PRIMARY KEY,
 request_id TEXT NOT NULL,
 attempt_id TEXT NOT NULL UNIQUE,
 session_binding TEXT NOT NULL,
 request_revision INTEGER NOT NULL,
 provider TEXT NOT NULL,
 verb TEXT NOT NULL,
 connection_id TEXT,
 target_id TEXT NOT NULL,
 expected_credential_revision INTEGER,
 expected_authorization_revision INTEGER,
 custody_epoch INTEGER NOT NULL,
 context_digest TEXT NOT NULL,
 issued_at_ms INTEGER NOT NULL,
 expires_at_ms INTEGER NOT NULL,
 claimed_at_ms INTEGER);
CREATE INDEX IF NOT EXISTS credential_request_nonces_request
 ON credential_request_nonces(request_id, claimed_at_ms);
CREATE TABLE IF NOT EXISTS credential_request_attempts(
 attempt_id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL,
 nonce_digest TEXT NOT NULL UNIQUE,
 session_binding TEXT NOT NULL,
 verb TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('claimed','applied','refused','unknown')),
 result_code TEXT,
 claimed_at_ms INTEGER NOT NULL,
 finished_at_ms INTEGER);
CREATE INDEX IF NOT EXISTS credential_request_attempts_request
 ON credential_request_attempts(request_id, claimed_at_ms);
CREATE TRIGGER IF NOT EXISTS credential_requests_no_delete
 BEFORE DELETE ON credential_requests
 BEGIN SELECT RAISE(ABORT,'credential request history is retained'); END;
CREATE TRIGGER IF NOT EXISTS credential_request_nonce_once
 BEFORE UPDATE OF claimed_at_ms ON credential_request_nonces
 WHEN OLD.claimed_at_ms IS NOT NULL AND NEW.claimed_at_ms IS NOT OLD.claimed_at_ms
 BEGIN SELECT RAISE(ABORT,'form nonce already claimed'); END;
CREATE TRIGGER IF NOT EXISTS credential_request_attempts_no_delete
 BEFORE DELETE ON credential_request_attempts
 BEGIN SELECT RAISE(ABORT,'credential attempt history is retained'); END;
CREATE TABLE IF NOT EXISTS protected_management_attempts(
 nonce_digest TEXT PRIMARY KEY,
 attempt_id TEXT NOT NULL UNIQUE,
 action TEXT NOT NULL CHECK(length(action) BETWEEN 1 AND 64),
 params_digest TEXT NOT NULL CHECK(length(params_digest) BETWEEN 16 AND 128),
 session_binding TEXT NOT NULL,
 custody_epoch INTEGER NOT NULL CHECK(custody_epoch>0),
 issued_at_ms INTEGER NOT NULL,
 expires_at_ms INTEGER NOT NULL,
 claimed_at_ms INTEGER,
 state TEXT NOT NULL CHECK(state IN('issued','claimed','applied','refused','unknown')),
 result_code TEXT,
 finished_at_ms INTEGER);
CREATE INDEX IF NOT EXISTS protected_management_attempts_session
 ON protected_management_attempts(session_binding,issued_at_ms);
CREATE TRIGGER IF NOT EXISTS protected_management_nonce_once
 BEFORE UPDATE OF claimed_at_ms ON protected_management_attempts
 WHEN OLD.claimed_at_ms IS NOT NULL AND NEW.claimed_at_ms IS NOT OLD.claimed_at_ms
 BEGIN SELECT RAISE(ABORT,'management nonce already claimed'); END;
CREATE TRIGGER IF NOT EXISTS protected_management_attempts_no_delete
 BEFORE DELETE ON protected_management_attempts
 BEGIN SELECT RAISE(ABORT,'management attempt history is retained'); END;
";

/// How long a request may wait for its human before it expires.
pub(crate) const REQUEST_TTL_MS: i64 = 30 * 60 * 1000;

/// How long one issued form nonce stays usable.
pub(crate) const NONCE_TTL_MS: i64 = 60 * 1000;

/// The largest credential the trusted form may submit.
pub(crate) const MAX_SECRET_BYTES: usize = 65_536;

/// Fixed state words (r3 §F3).
pub(crate) const STATE_PENDING: &str = "pending";
pub(crate) const STATE_SUBMITTING: &str = "submitting";
pub(crate) const STATE_DONE: &str = "done";
pub(crate) const STATE_CANCELLED: &str = "cancelled";
pub(crate) const STATE_EXPIRED: &str = "expired";
pub(crate) const STATE_UNKNOWN: &str = "unknown";

/// Fixed status words the entry page and chat card speak.
pub(crate) const STATUS_AWAITING_HUMAN: &str = "awaiting_human";
pub(crate) const STATUS_SUBMITTING: &str = "submitting";
pub(crate) const STATUS_SAVED: &str = "saved";
pub(crate) const STATUS_VERIFIED: &str = "verified";
pub(crate) const STATUS_REFUSED: &str = "refused";
pub(crate) const STATUS_CANCELLED: &str = "cancelled";
pub(crate) const STATUS_EXPIRED: &str = "expired";
pub(crate) const STATUS_UNKNOWN: &str = "unknown";

/// The fixed human verb a request allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialVerb {
    Enroll,
    Rotate,
    Verify,
}

impl CredentialVerb {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Enroll => "enroll",
            Self::Rotate => "rotate",
            Self::Verify => "verify",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "enroll" => Ok(Self::Enroll),
            "rotate" => Ok(Self::Rotate),
            "verify" => Ok(Self::Verify),
            _ => Err(Error::invalid(
                "invalid_request",
                "unknown credential request verb",
            )),
        }
    }
}

/// A fresh request. Every field is an identifier or the protected context
/// digest; no credential or human-entered metadata appears here.
#[derive(Clone, Debug)]
pub(crate) struct NewCredentialRequest {
    pub provider: String,
    pub verb: CredentialVerb,
    pub connection_id: Option<String>,
    pub shape: Option<String>,
    pub scopes: Vec<String>,
    pub expected_credential_revision: Option<i64>,
    pub expected_authorization_revision: Option<i64>,
    pub requester_kind: String,
    pub requester_participant: Option<String>,
    pub conversation_id: Option<String>,
    pub target_agent_uid: Option<String>,
    pub custody_domain: String,
    pub custody_epoch: i64,
    pub context_digest: String,
    /// A server-resolved opaque target for a source that is not a business
    /// Connection (the native-model broker's own source id). Never browser
    /// text and never a credential.
    pub target_id_override: Option<String>,
}

/// One durable request row, metadata only.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CredentialRequest {
    pub request_id: String,
    pub request_revision: i64,
    pub provider: String,
    pub verb: String,
    pub connection_id: Option<String>,
    /// Opaque target account for a fresh enrollment; the connection id for
    /// rotate/verify. The human-facing alias is derived from it.
    pub target_id: String,
    pub shape: Option<String>,
    pub scopes: Vec<String>,
    pub expected_credential_revision: Option<i64>,
    pub expected_authorization_revision: Option<i64>,
    pub custody_domain: String,
    pub custody_epoch: i64,
    pub context_digest: String,
    pub requester_kind: String,
    pub requester_participant: Option<String>,
    pub conversation_id: Option<String>,
    pub target_agent_uid: Option<String>,
    pub state: String,
    pub status: String,
    pub result_code: Option<String>,
    pub result_connection_id: Option<String>,
    pub verification_request_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub expires_at_ms: i64,
}

impl CredentialRequest {
    /// The effective state for display: an expired pending request reads
    /// `expired` without a write. Nothing here grants authority.
    pub(crate) fn effective_state(&self, now_ms: i64) -> &str {
        if self.state == STATE_PENDING && now_ms >= self.expires_at_ms {
            STATE_EXPIRED
        } else {
            self.state.as_str()
        }
    }
    /// The human-facing account alias for a target. The opaque id itself is
    /// the only durable form.
    pub(crate) fn display_alias(&self) -> String {
        match self.verb.as_str() {
            "enroll" => format!("new {}", self.provider),
            _ => self.target_id.clone(),
        }
    }
    /// Metadata-only wire shape. Never carries a credential, digest, entry
    /// session binding or pre-submit transport/username value.
    pub(crate) fn to_json(&self, now_ms: i64) -> Value {
        let state = self.effective_state(now_ms);
        let status = if state == STATE_EXPIRED && self.state == STATE_PENDING {
            STATUS_EXPIRED
        } else {
            self.status.as_str()
        };
        json!({
            "request_id": self.request_id,
            "request_revision": self.request_revision,
            "provider": self.provider,
            "verb": self.verb,
            "connection_id": self.connection_id.as_deref().or(self.result_connection_id.as_deref()),
            "target_id": self.target_id,
            "display_alias": self.display_alias(),
            "shape": self.shape,
            "scopes": self.scopes,
            "expected_credential_revision": self.expected_credential_revision,
            "expected_authorization_revision": self.expected_authorization_revision,
            "custody_domain": self.custody_domain,
            "custody_epoch": self.custody_epoch,
            "context_digest": self.context_digest,
            "requester_kind": self.requester_kind,
            "requester_participant": self.requester_participant,
            "conversation_id": self.conversation_id,
            "target_agent_uid": self.target_agent_uid,
            "state": state,
            "status": status,
            "result_code": self.result_code,
            "verification_request_id": self.verification_request_id,
            "created_at_ms": self.created_at_ms,
            "updated_at_ms": self.updated_at_ms,
            "expires_at_ms": self.expires_at_ms,
            "entry_path": format!("/connections/enroll/{}", self.request_id),
        })
    }
}

/// A claimed attempt: the exact request it was claimed for. The daemon
/// turns this into the private `VerifiedHumanAttemptV1`; it is never
/// serialized.
#[derive(Clone, Debug)]
pub(crate) struct ClaimedCredentialAttempt {
    pub request: CredentialRequest,
    pub attempt_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttemptOutcome {
    Applied,
    Refused,
    Unknown,
}

fn identifier(value: &str, what: &str) -> Result<String> {
    crate::proto::identifier(value, what)
}

fn parse_scopes(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn now_i64(value: i64) -> Result<i64> {
    if value <= 0 {
        return Err(Error::internal("credential request clock is unavailable"));
    }
    Ok(value)
}

fn request_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CredentialRequest> {
    let scopes: String = row.get("scopes")?;
    Ok(CredentialRequest {
        request_id: row.get("request_id")?,
        request_revision: row.get("request_revision")?,
        provider: row.get("provider")?,
        verb: row.get("verb")?,
        connection_id: row.get("connection_id")?,
        target_id: row.get("target_id")?,
        shape: row.get("enrollment_shape")?,
        scopes: parse_scopes(&scopes),
        expected_credential_revision: row.get("expected_credential_revision")?,
        expected_authorization_revision: row.get("expected_authorization_revision")?,
        custody_domain: row.get("custody_domain")?,
        custody_epoch: row.get("custody_epoch")?,
        context_digest: row.get("context_digest")?,
        requester_kind: row.get("requester_kind")?,
        requester_participant: row.get("requester_participant")?,
        conversation_id: row.get("conversation_id")?,
        target_agent_uid: row.get("target_agent_uid")?,
        state: row.get("state")?,
        status: row.get("status")?,
        result_code: row.get("result_code")?,
        result_connection_id: row.get("result_connection_id")?,
        verification_request_id: row.get("verification_request_id")?,
        created_at_ms: row.get("created_at_ms")?,
        updated_at_ms: row.get("updated_at_ms")?,
        expires_at_ms: row.get("expires_at_ms")?,
    })
}

fn load_request(tx: &impl StoreConn, id: &str) -> Result<Option<CredentialRequest>> {
    tx.query_opt(
        "SELECT * FROM credential_requests WHERE request_id=?",
        [id],
        request_row,
    )
    .map_err(Error::from)
}

fn current_credential_revision(tx: &impl StoreConn, connection_id: &str) -> Result<Option<i64>> {
    tx.query_opt(
        "SELECT credential_revision FROM platform_credentials WHERE connection_id=?",
        [connection_id],
        |row| row.get::<_, i64>(0),
    )
    .map_err(Error::from)
}

/// Re-check the exact live target the request was bound to. Native model
/// sources and business Connections are distinct authorities; neither can
/// substitute for the other's credential revision.
fn current_revision_matches(request: &CredentialRequest, tx: &impl StoreConn) -> Result<bool> {
    let Some(expected) = request.expected_credential_revision else {
        return Ok(!is_native_model_shape(request.shape.as_deref())
            || request.verb == CredentialVerb::Enroll.as_str());
    };
    if is_native_model_shape(request.shape.as_deref()) {
        return super::native_model_turn::rotation_source_matches_in(
            tx,
            &request.target_id,
            &request.provider,
            expected,
        );
    }
    let Some(connection_id) = request.connection_id.as_deref() else {
        return Ok(false);
    };
    Ok(current_credential_revision(tx, connection_id)? == Some(expected))
}

/// The exact isolated-form shapes that belong to the native-model source, not
/// a business Connection. They route to the model broker and never create a
/// Connection/vault row.
pub(crate) const NATIVE_MODEL_SHAPES: &[&str] =
    &["native-model-api-key", "native-model-devin-credentials"];

pub(crate) fn is_native_model_shape(shape: Option<&str>) -> bool {
    shape.is_some_and(|shape| NATIVE_MODEL_SHAPES.contains(&shape))
}

fn validate_new(new: &NewCredentialRequest) -> Result<()> {
    if new.requester_kind != "operator_cli"
        && new.requester_kind != "operator_ui"
        && new.requester_kind != "agent"
    {
        return Err(Error::invalid(
            "invalid_request",
            "unknown credential requester",
        ));
    }
    let native_shape = is_native_model_shape(new.shape.as_deref());
    if native_shape {
        // Exactly the bounded native-model source pair. It is not a business
        // Connection: no connection id, no scopes, provider exactly `devin`.
        if new.provider != "devin" {
            return Err(Error::invalid(
                "invalid_request",
                "this provider does not enroll that shape",
            ));
        }
        if new.connection_id.is_some() || !new.scopes.is_empty() {
            return Err(Error::invalid(
                "invalid_request",
                "the native model source takes no connection or scope",
            ));
        }
        if new.verb != CredentialVerb::Enroll {
            match new.target_id_override.as_deref() {
                Some(source) => {
                    identifier(source, "Source")?;
                }
                None => {
                    return Err(Error::invalid(
                        "invalid_request",
                        "the native model source is required",
                    ))
                }
            }
        }
    } else {
        match new.verb {
            CredentialVerb::Enroll => {
                if new.connection_id.is_some() {
                    return Err(Error::invalid(
                        "invalid_request",
                        "an enrollment request names no existing connection",
                    ));
                }
            }
            CredentialVerb::Rotate | CredentialVerb::Verify => {
                let id = new.connection_id.as_deref().ok_or_else(|| {
                    Error::invalid("invalid_request", "this request needs a connection")
                })?;
                identifier(id, "Connection")?;
            }
        }
        if new.verb == CredentialVerb::Verify
            && !matches!(new.shape.as_deref(), None | Some("smtp"))
        {
            return Err(Error::invalid(
                "unsupported_verification",
                "this provider has no reviewed verification adapter",
            ));
        }
        if new.verb == CredentialVerb::Enroll && new.shape.is_none() {
            return Err(Error::invalid(
                "invalid_request",
                "an enrollment request needs its shape",
            ));
        }
        if let Some(shape) = new.shape.as_deref() {
            if !matches!(shape, "token" | "smtp") {
                return Err(Error::invalid(
                    "invalid_request",
                    "unknown enrollment shape",
                ));
            }
        }
    }
    if new.verb != CredentialVerb::Enroll && new.expected_credential_revision.is_none() {
        return Err(Error::invalid(
            "invalid_request",
            "this request needs the current credential revision",
        ));
    }
    if let Some(revision) = new.expected_credential_revision {
        if revision <= 0 {
            return Err(Error::invalid(
                "invalid_request",
                "invalid credential revision",
            ));
        }
    }
    if let Some(revision) = new.expected_authorization_revision {
        if revision <= 0 {
            return Err(Error::invalid(
                "invalid_request",
                "invalid authorization revision",
            ));
        }
    }
    if new.scopes.len() > 32 {
        return Err(Error::invalid("invalid_request", "too many scopes"));
    }
    for scope in &new.scopes {
        identifier(scope, "Scope")?;
    }
    if new.custody_epoch <= 0 {
        return Err(Error::invalid(
            "request_not_ready",
            "protected custody has no live epoch",
        ));
    }
    if new.context_digest.len() < 16 || new.context_digest.len() > 128 {
        return Err(Error::invalid(
            "request_not_ready",
            "the protected entry context is unavailable",
        ));
    }
    if let Some(uid) = new.requester_participant.as_deref() {
        if !uid.starts_with("agt-") || uid.len() > 128 {
            return Err(Error::invalid(
                "invalid_request",
                "agent participant identity is invalid",
            ));
        }
    }
    Ok(())
}

impl Store {
    /// Create one metadata-only request. The caller has already resolved
    /// the live protected context through the parent runtime owner; this
    /// store never invents an epoch or falls back to metadata.
    pub(crate) fn create_credential_request(
        &self,
        new: NewCredentialRequest,
        now_ms: i64,
    ) -> Result<CredentialRequest> {
        let now_ms = now_i64(now_ms)?;
        validate_new(&new)?;
        let provider =
            crate::platform::connections::provider_identifier(&new.provider, "Provider")?;
        let request_id = uuid::Uuid::new_v4().to_string();
        // A fresh enrollment's account handle is opaque and server-chosen;
        // no operator/machine text (which might equal the password) is
        // persisted before the secret is screened with it.
        let target_id = match new.target_id_override.clone() {
            Some(target) => target,
            None => match new.connection_id.as_deref() {
                Some(connection_id) => connection_id.to_string(),
                None => format!("acct-{}", uuid::Uuid::new_v4().simple()),
            },
        };
        let scopes = serde_json::to_string(&new.scopes).unwrap_or_else(|_| "[]".to_string());
        let verification_request_id = if new.verb == CredentialVerb::Verify {
            Some(request_id.clone())
        } else {
            None
        };
        self.write_tx(move |tx| {
            tx.execute(
                "INSERT INTO credential_requests(
                   request_id, request_revision, provider, verb, connection_id, target_id,
                   enrollment_shape, scopes, expected_credential_revision,
                   expected_authorization_revision, custody_domain, custody_epoch,
                   context_digest, requester_kind, requester_participant, conversation_id,
                   target_agent_uid, state, status, result_code, verification_request_id,
                   created_at_ms, updated_at_ms, expires_at_ms)
                 VALUES(?1,1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                   'pending',?17,NULL,?18,?19,?19,?20)",
                params![
                    request_id,
                    provider,
                    new.verb.as_str(),
                    new.connection_id,
                    target_id,
                    new.shape,
                    scopes,
                    new.expected_credential_revision,
                    new.expected_authorization_revision,
                    new.custody_domain,
                    new.custody_epoch,
                    new.context_digest,
                    new.requester_kind,
                    new.requester_participant,
                    new.conversation_id,
                    new.target_agent_uid,
                    STATUS_AWAITING_HUMAN,
                    verification_request_id,
                    now_ms,
                    now_ms + REQUEST_TTL_MS,
                ],
            )?;
            load_request(tx, &request_id)?
                .ok_or_else(|| Error::internal("credential request did not persist"))
        })
    }

    /// One request, metadata only.
    pub(crate) fn credential_request(&self, request_id: &str) -> Result<Option<CredentialRequest>> {
        let conn = self.conn();
        load_request(&*conn, request_id)
    }

    /// Requests asked from one conversation, newest first.
    pub(crate) fn credential_requests_for_conversation(
        &self,
        conversation_id: &str,
        limit: i64,
    ) -> Result<Vec<CredentialRequest>> {
        let limit = limit.clamp(1, 50);
        let conn = self.conn();
        conn.query_vec(
            "SELECT * FROM credential_requests WHERE conversation_id=? \
             ORDER BY created_at_ms DESC, request_id LIMIT ?",
            params![conversation_id, limit],
            request_row,
        )
        .map_err(Error::from)
    }

    /// The most recent requests, metadata only.
    pub(crate) fn recent_credential_requests(&self, limit: i64) -> Result<Vec<CredentialRequest>> {
        let limit = limit.clamp(1, 50);
        let conn = self.conn();
        conn.query_vec(
            "SELECT * FROM credential_requests ORDER BY created_at_ms DESC, request_id LIMIT ?",
            params![limit],
            request_row,
        )
        .map_err(Error::from)
    }

    /// Mint one form nonce for this live authenticated owner session. The
    /// raw 256-bit value is returned exactly once; only its digest is
    /// stored. Any earlier unclaimed nonce for the same request/session is
    /// invalidated so only the newest form can submit.
    pub(crate) fn issue_credential_nonce(
        &self,
        request_id: &str,
        session_binding: &str,
        custody_epoch: i64,
        context_digest: &str,
        now_ms: i64,
    ) -> Result<(String, CredentialRequest)> {
        let now_ms = now_i64(now_ms)?;
        if session_binding.is_empty() || session_binding.len() > 128 {
            return Err(Error::invalid(
                "entry_session_required",
                "entry session is unavailable",
            ));
        }
        let mut nonce_bytes = [0u8; 32];
        getrandom::fill(&mut nonce_bytes)
            .map_err(|_| Error::internal("entropy for a form nonce is unavailable"))?;
        let nonce: String = nonce_bytes.iter().map(|b| format!("{b:02x}")).collect();
        let digest = crate::operator_auth::digest(&nonce);
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let request_id = request_id.to_owned();
        let session_binding = session_binding.to_owned();
        let context_digest = context_digest.to_owned();
        let (nonce, request) = self.write_tx(move |tx| {
            let request = load_request(tx, &request_id)?.ok_or_else(|| {
                Error::invalid("request_changed", "credential request is unavailable")
            })?;
            if request.effective_state(now_ms) != STATE_PENDING {
                return Err(Error::invalid(
                    "request_changed",
                    "credential request is not awaiting a human",
                ));
            }
            if request.custody_epoch != custody_epoch || request.context_digest != context_digest {
                return Err(Error::invalid(
                    "request_changed",
                    "the protected context changed; start a new request",
                ));
            }
            if !current_revision_matches(&request, tx)? {
                return Err(Error::invalid(
                    "request_changed",
                    "the connection changed; start a new request",
                ));
            }
            // A reload supersedes the earlier form: expire it so a late
            // submit is `nonce_expired`, not mistaken for a used form.
            tx.execute(
                "UPDATE credential_request_nonces SET expires_at_ms=?1 \
                 WHERE request_id=?2 AND session_binding=?3 AND claimed_at_ms IS NULL",
                params![now_ms, request.request_id, session_binding],
            )?;
            tx.execute(
                "INSERT INTO credential_request_nonces(
                   nonce_digest, request_id, attempt_id, session_binding, request_revision,
                   provider, verb, connection_id, target_id, expected_credential_revision,
                   expected_authorization_revision, custody_epoch, context_digest,
                   issued_at_ms, expires_at_ms, claimed_at_ms)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,NULL)",
                params![
                    digest,
                    request.request_id,
                    attempt_id,
                    session_binding,
                    request.request_revision,
                    request.provider,
                    request.verb,
                    request.connection_id,
                    request.target_id,
                    request.expected_credential_revision,
                    request.expected_authorization_revision,
                    request.custody_epoch,
                    request.context_digest,
                    now_ms,
                    now_ms + NONCE_TTL_MS,
                ],
            )?;
            Ok((nonce, request))
        })?;
        Ok((nonce, request))
    }

    /// Claim one nonce and open its attempt in the same transaction —
    /// before the daemon forwards anything. Every failure is a fixed code;
    /// a lost claim can never be retried into a second mutation.
    pub(crate) fn claim_credential_attempt(
        &self,
        request_id: &str,
        nonce: &str,
        session_binding: &str,
        custody_epoch: i64,
        context_digest: &str,
        now_ms: i64,
    ) -> Result<ClaimedCredentialAttempt> {
        let now_ms = now_i64(now_ms)?;
        if nonce.is_empty() || nonce.len() > 128 {
            return Err(Error::invalid(
                "nonce_expired",
                "the form expired; reload it to continue",
            ));
        }
        if session_binding.is_empty() || session_binding.len() > 128 {
            return Err(Error::invalid(
                "entry_session_required",
                "entry session is unavailable",
            ));
        }
        let digest = crate::operator_auth::digest(nonce);
        let request_id = request_id.to_owned();
        let session_binding = session_binding.to_owned();
        let context_digest = context_digest.to_owned();
        self.write_tx(move |tx| {
            let request = load_request(tx, &request_id)?.ok_or_else(|| {
                Error::invalid("request_changed", "credential request is unavailable")
            })?;
            if request.effective_state(now_ms) == STATE_EXPIRED {
                tx.execute(
                    "UPDATE credential_requests SET state='expired', status=?1, updated_at_ms=?2 \
                     WHERE request_id=?3 AND state='pending'",
                    params![STATUS_EXPIRED, now_ms, request.request_id],
                )?;
                return Err(Error::invalid(
                    "nonce_expired",
                    "the form expired; start a new request",
                ));
            }
            if request.state != STATE_PENDING {
                return if request.state == STATE_SUBMITTING || request.state == STATE_DONE {
                    Err(Error::invalid(
                        "already_submitted",
                        "this form was already submitted",
                    ))
                } else {
                    Err(Error::invalid(
                        "request_changed",
                        "credential request is no longer open",
                    ))
                };
            }
            let nonce_row = tx
                .query_opt(
                    "SELECT request_id, session_binding, request_revision, provider, verb, \
                            connection_id, target_id, expected_credential_revision, \
                            expected_authorization_revision, custody_epoch, context_digest, \
                            expires_at_ms, claimed_at_ms, attempt_id \
                     FROM credential_request_nonces WHERE nonce_digest=?",
                    [&digest],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, String>(6)?,
                            row.get::<_, Option<i64>>(7)?,
                            row.get::<_, Option<i64>>(8)?,
                            row.get::<_, i64>(9)?,
                            row.get::<_, String>(10)?,
                            row.get::<_, i64>(11)?,
                            row.get::<_, Option<i64>>(12)?,
                            row.get::<_, String>(13)?,
                        ))
                    },
                )
                .map_err(Error::from)?;
            let Some((
                nonce_request,
                nonce_session,
                nonce_revision,
                nonce_provider,
                nonce_verb,
                nonce_connection,
                nonce_target,
                nonce_credential,
                nonce_authorization,
                nonce_epoch,
                nonce_context,
                nonce_expires,
                claimed,
                attempt_id,
            )) = nonce_row
            else {
                return Err(Error::invalid(
                    "nonce_expired",
                    "the form expired; reload it to continue",
                ));
            };
            if nonce_request != request.request_id || nonce_session != session_binding {
                return Err(Error::invalid(
                    "request_changed",
                    "the form does not belong to this request",
                ));
            }
            if claimed.is_some() {
                return Err(Error::invalid(
                    "already_submitted",
                    "this form was already submitted",
                ));
            }
            if now_ms >= nonce_expires {
                return Err(Error::invalid(
                    "nonce_expired",
                    "the form expired; reload it to continue",
                ));
            }
            if nonce_revision != request.request_revision
                || nonce_provider != request.provider
                || nonce_verb != request.verb
                || nonce_connection != request.connection_id
                || nonce_target != request.target_id
                || nonce_credential != request.expected_credential_revision
                || nonce_authorization != request.expected_authorization_revision
                || nonce_epoch != request.custody_epoch
                || nonce_context != request.context_digest
                || request.custody_epoch != custody_epoch
                || request.context_digest != context_digest
            {
                return Err(Error::invalid(
                    "request_changed",
                    "the form does not match this request",
                ));
            }
            if !current_revision_matches(&request, tx)? {
                return Err(Error::invalid(
                    "request_changed",
                    "the connection changed; start a new request",
                ));
            }
            let claimed_rows = tx.execute(
                "UPDATE credential_request_nonces SET claimed_at_ms=?1 \
                 WHERE nonce_digest=?2 AND claimed_at_ms IS NULL AND expires_at_ms>?1",
                params![now_ms, digest],
            )?;
            if claimed_rows != 1 {
                return Err(Error::invalid(
                    "already_submitted",
                    "this form was already submitted",
                ));
            }
            let opened = tx.execute(
                "UPDATE credential_requests SET state='submitting', status=?1, updated_at_ms=?2 \
                 WHERE request_id=?3 AND state='pending'",
                params![STATUS_SUBMITTING, now_ms, request.request_id],
            )?;
            if opened != 1 {
                return Err(Error::invalid(
                    "request_changed",
                    "credential request is no longer open",
                ));
            }
            tx.execute(
                "INSERT INTO credential_request_attempts(
                   attempt_id, request_id, nonce_digest, session_binding, verb, state,
                   result_code, claimed_at_ms, finished_at_ms)
                 VALUES(?1,?2,?3,?4,?5,'claimed',NULL,?6,NULL)",
                params![
                    attempt_id,
                    request.request_id,
                    digest,
                    session_binding,
                    request.verb,
                    now_ms
                ],
            )?;
            Ok(ClaimedCredentialAttempt {
                request,
                attempt_id,
            })
        })
    }

    /// Close a claimed attempt. The request returns to `pending` after a
    /// definite refusal so the human may try a new form; a lost or
    /// ambiguous outcome stays `unknown` and is never retried.
    pub(crate) fn finish_credential_attempt(
        &self,
        request_id: &str,
        attempt_id: &str,
        outcome: AttemptOutcome,
        code: Option<&str>,
        applied_connection_id: Option<&str>,
        now_ms: i64,
    ) -> Result<()> {
        let now_ms = now_i64(now_ms)?;
        let request_id = request_id.to_owned();
        let attempt_id = attempt_id.to_owned();
        let code = code.map(str::to_owned);
        let applied_connection_id = if outcome == AttemptOutcome::Applied {
            applied_connection_id.map(str::to_owned)
        } else {
            None
        };
        self.write_tx(move |tx| {
            let (attempt_state, attempt_code) = match outcome {
                AttemptOutcome::Applied => ("applied", None),
                AttemptOutcome::Refused => ("refused", code.clone()),
                AttemptOutcome::Unknown => ("unknown", code.clone()),
            };
            tx.execute(
                "UPDATE credential_request_attempts SET state=?1, result_code=?2, finished_at_ms=?3 \
                 WHERE attempt_id=?4 AND state='claimed'",
                params![attempt_state, attempt_code, now_ms, attempt_id],
            )?;
            let status = match outcome {
                AttemptOutcome::Applied => {
                    let verb: String = tx
                        .query_row(
                            "SELECT verb FROM credential_requests WHERE request_id=?",
                            [&request_id],
                            |row| row.get(0),
                        )
                        .map_err(Error::from)?;
                    if verb == "verify" {
                        STATUS_VERIFIED
                    } else {
                        STATUS_SAVED
                    }
                }
                AttemptOutcome::Refused => STATUS_REFUSED,
                AttemptOutcome::Unknown => STATUS_UNKNOWN,
            };
            let (state, result_code) = match outcome {
                AttemptOutcome::Applied => (STATE_DONE, None),
                AttemptOutcome::Refused => (STATE_PENDING, code.clone()),
                AttemptOutcome::Unknown => (STATE_UNKNOWN, code.clone()),
            };
            // A definite refusal reopens the request only from the exact
            // `submitting` state this attempt opened; a concurrent cancel
            // or expiry is never overwritten.
            let changed = tx.execute(
                "UPDATE credential_requests SET state=?1, status=?2, result_code=?3, updated_at_ms=?4, \
                 result_connection_id=COALESCE(result_connection_id,?5) \
                 WHERE request_id=?6 AND state='submitting'",
                params![state, status, result_code, now_ms, applied_connection_id, request_id],
            )?;
            if changed == 0 && outcome != AttemptOutcome::Unknown {
                return Err(Error::invalid(
                    "request_changed",
                    "credential request closed before the attempt finished",
                ));
            }
            Ok(())
        })
    }

    /// Cancel an open request. A claimed attempt may be in flight and is
    /// never promised undone.
    pub(crate) fn cancel_credential_request(
        &self,
        request_id: &str,
        now_ms: i64,
    ) -> Result<CredentialRequest> {
        let now_ms = now_i64(now_ms)?;
        let request_id = request_id.to_owned();
        self.write_tx(move |tx| {
            let request = load_request(tx, &request_id)?.ok_or_else(|| {
                Error::invalid("request_changed", "credential request is unavailable")
            })?;
            match request.state.as_str() {
                STATE_PENDING => {
                    tx.execute(
                        "UPDATE credential_requests SET state='cancelled', status=?1, updated_at_ms=?2 \
                         WHERE request_id=?3 AND state='pending'",
                        params![STATUS_CANCELLED, now_ms, request.request_id],
                    )?;
                    tx.execute(
                        "UPDATE credential_request_nonces SET expires_at_ms=?1 \
                         WHERE request_id=?2 AND claimed_at_ms IS NULL",
                        params![now_ms, request.request_id],
                    )?;
                }
                STATE_CANCELLED => {}
                STATE_SUBMITTING | STATE_DONE => {
                    return Err(Error::invalid(
                        "already_submitted",
                        "this request was already submitted",
                    ))
                }
                _ => {
                    return Err(Error::invalid(
                        "request_changed",
                        "credential request is no longer open",
                    ))
                }
            }
            load_request(tx, &request_id)?
                .ok_or_else(|| Error::internal("credential request did not persist"))
        })
    }

    // -- protected management attempts -------------------------------------

    /// Issue ONE 60s one-use nonce for a typed management action, bound to the
    /// live authenticated owner session, the action and the exact canonical
    /// parameters digest. No GET mints state and no nonce is reusable.
    pub(crate) fn issue_management_nonce(
        &self,
        action: &str,
        params_digest: &str,
        session_binding: &str,
        custody_epoch: i64,
        now_ms: i64,
    ) -> Result<(String, String, i64)> {
        let now_ms = now_i64(now_ms)?;
        if !identifier_shape(action)
            || params_digest.len() < 16
            || params_digest.len() > 128
            || session_binding.is_empty()
            || session_binding.len() > 128
            || custody_epoch <= 0
        {
            return Err(Error::invalid(
                "invalid_request",
                "the management attempt is malformed",
            ));
        }
        let mut nonce_bytes = [0u8; 32];
        getrandom::fill(&mut nonce_bytes)
            .map_err(|_| Error::internal("entropy for a management nonce is unavailable"))?;
        let nonce: String = nonce_bytes.iter().map(|b| format!("{b:02x}")).collect();
        let digest = crate::operator_auth::digest(&nonce);
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let action = action.to_owned();
        let params_digest = params_digest.to_owned();
        let session_binding = session_binding.to_owned();
        let expires = now_ms + NONCE_TTL_MS;
        let (attempt, base) = (attempt_id.clone(), nonce.clone());
        self.write_tx(move |tx| {
            // A fresh issue supersedes the earlier outstanding attempt for the
            // same session/action so a late submit is `nonce_expired`.
            tx.execute(
                "UPDATE protected_management_attempts SET expires_at_ms=?1 \
                 WHERE session_binding=?2 AND action=?3 AND claimed_at_ms IS NULL",
                params![now_ms, session_binding, action],
            )?;
            tx.execute(
                "INSERT INTO protected_management_attempts(
                   nonce_digest,attempt_id,action,params_digest,session_binding,custody_epoch,
                   issued_at_ms,expires_at_ms,claimed_at_ms,state,result_code,finished_at_ms)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,NULL,'issued',NULL,NULL)",
                params![
                    digest,
                    attempt,
                    action,
                    params_digest,
                    session_binding,
                    custody_epoch,
                    now_ms,
                    expires
                ],
            )?;
            Ok(())
        })?;
        Ok((base, attempt_id, expires))
    }

    /// Claim the exact nonce once before the mutation. Every binding is
    /// re-checked; a replayed, expired or changed attempt refuses.
    #[allow(clippy::too_many_arguments, clippy::type_complexity)] // the attempt row mirrors the table
    pub(crate) fn claim_management_attempt(
        &self,
        nonce: &str,
        expected_attempt_id: &str,
        action: &str,
        params_digest: &str,
        session_binding: &str,
        custody_epoch: i64,
        now_ms: i64,
    ) -> Result<String> {
        let now_ms = now_i64(now_ms)?;
        let digest = crate::operator_auth::digest(nonce);
        let expected_attempt_id = expected_attempt_id.to_owned();
        let action = action.to_owned();
        let params_digest = params_digest.to_owned();
        let session_binding = session_binding.to_owned();
        self.write_tx(move |tx| {
            let row: Option<(String, String, String, String, i64, i64, Option<i64>)> = tx
                .query_opt(
                    "SELECT attempt_id,action,params_digest,session_binding,custody_epoch,\
                     expires_at_ms,claimed_at_ms FROM protected_management_attempts \
                     WHERE nonce_digest=?1",
                    params![digest],
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
                )?;
            let Some((
                attempt_id,
                stored_action,
                stored_digest,
                stored_session,
                stored_epoch,
                expires,
                claimed,
            )) = row
            else {
                return Err(Error::invalid(
                    "entry_session_required",
                    "management attempt is unavailable",
                ));
            };
            if attempt_id != expected_attempt_id
                || stored_action != action
                || stored_digest != params_digest
                || stored_session != session_binding
                || stored_epoch != custody_epoch
            {
                return Err(Error::invalid(
                    "request_changed",
                    "the management attempt changed",
                ));
            }
            if claimed.is_some() {
                return Err(Error::invalid(
                    "already_submitted",
                    "this management attempt was already used",
                ));
            }
            if now_ms >= expires {
                return Err(Error::invalid(
                    "nonce_expired",
                    "the management attempt expired",
                ));
            }
            let updated = tx.execute_raw(
                "UPDATE protected_management_attempts SET claimed_at_ms=?1,state='claimed' \
                 WHERE nonce_digest=?2 AND claimed_at_ms IS NULL",
                params![now_ms, digest],
            )?;
            if updated != 1 {
                return Err(Error::invalid(
                    "already_submitted",
                    "this management attempt was already used",
                ));
            }
            Ok(attempt_id)
        })
    }

    /// Record the terminal attempt status. `applied` means the mutation AND
    /// its external checkpoint succeeded; unknown is never success.
    pub(crate) fn finish_management_attempt(
        &self,
        attempt_id: &str,
        applied: bool,
        unknown: bool,
        code: Option<&str>,
        now_ms: i64,
    ) -> Result<()> {
        let now_ms = now_i64(now_ms)?;
        let attempt_id = attempt_id.to_owned();
        let state = if applied {
            "applied"
        } else if unknown {
            "unknown"
        } else {
            "refused"
        };
        let code = code.map(str::to_owned);
        self.write_tx(move |tx| {
            let changed = tx.execute(
                "UPDATE protected_management_attempts SET state=?1,result_code=?2,finished_at_ms=?3 \
                 WHERE attempt_id=?4 AND state='claimed' AND finished_at_ms IS NULL",
                params![state, code, now_ms, attempt_id],
            )?;
            if changed != 1 {
                return Err(Error::invalid(
                    "request_changed",
                    "the management attempt is not the claimed action",
                ));
            }
            Ok(())
        })
    }

    pub(crate) fn management_attempt(&self, attempt_id: &str) -> Result<Option<ManagementAttempt>> {
        let conn = self.conn();
        Ok(conn.query_opt(
            "SELECT attempt_id,action,state,result_code,expires_at_ms \
                 FROM protected_management_attempts WHERE attempt_id=?1",
            [attempt_id],
            |row| {
                Ok(ManagementAttempt {
                    attempt_id: row.get(0)?,
                    action: row.get(1)?,
                    state: row.get(2)?,
                    result_code: row.get(3)?,
                    expires_at_ms: row.get(4)?,
                })
            },
        )?)
    }
}

/// One durable protected-management attempt. Metadata only.
#[derive(Clone, Debug)]
pub(crate) struct ManagementAttempt {
    pub attempt_id: String,
    pub action: String,
    pub state: String,
    pub result_code: Option<String>,
    pub expires_at_ms: i64,
}

fn identifier_shape(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|b| (0x21..=0x7e).contains(&b))
}
