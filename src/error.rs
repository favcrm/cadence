//! Error taxonomy, mirroring the reference implementation.
//!
//! - [`Error::Rejected`]: the caller made an invalid or disallowed request.
//! - [`Error::Provider`]: the provider explicitly rejected a request.
//! - [`Error::OutcomeUnknown`]: the connection failed after a request could
//!   have reached the provider. The outcome must be preserved for review,
//!   never silently retried.
//! - [`Error::Internal`]: local runtime failures (I/O, storage, protocol).

use std::fmt;

/// Evidence captured around a missed pty render check — carried on
/// [`Error::NotRendered`] so the daemon's `paste_not_rendered` event
/// records what the pane actually showed, not just the verdict.
#[derive(Debug)]
pub struct RenderMiss {
    /// What the check concluded — dropped paste or unsubmitted draft.
    pub reason: String,
    /// Normalized screen tail before the paste (last 12 rows).
    pub before_tail: Vec<String>,
    /// Normalized screen tail after the render deadline (last 12 rows).
    pub after_tail: Vec<String>,
    /// The probe verdict that admitted the send at claim time, when
    /// one was recorded — what "idle" looked like to the gate.
    pub claim_probe: Option<serde_json::Value>,
    /// The re-probe taken after the render deadline (CAD-520): what the
    /// pane showed once more before the miss was accepted as real.
    pub reprobe: Option<serde_json::Value>,
}

/// A wire error that carries a stable `code` and, for revision
/// conflicts, the current revision. `kind` stays `rejected` for invalid
/// input and `conflict` when a caller must reload rather than retry.
#[derive(Debug)]
pub struct Structured {
    pub kind: &'static str,
    pub code: String,
    pub message: String,
    pub revision: Option<i64>,
}

#[derive(Debug)]
pub enum Error {
    Rejected(String),
    Provider(String),
    OutcomeUnknown(String),
    Internal(String),
    /// Invalid input or a revision conflict, with a stable code.
    Structured(Structured),
    /// The endpoint is not safe to submit to right now; the message
    /// returns to `queued` and is retried, never failed or pasted blind.
    GateRefused(String),
    /// Deterministic rejection made *before* any bytes could reach the
    /// provider — the message fails, but the endpoint is provably
    /// untouched, so the actor must not fence or close it.
    PreWrite(String),
    /// The paste was accepted by the terminal path but did not render
    /// within the deadline — evidence of a dropped or unsubmitted paste,
    /// not proof (pty post-paste screen check). The actor decides:
    /// routed notifications requeue bounded then park; task messages go
    /// `unknown` under the usual uncertainty discipline.
    NotRendered(RenderMiss),
}

impl Error {
    pub fn rejected(message: impl Into<String>) -> Self {
        Self::Rejected(message.into())
    }
    pub fn gate(message: impl Into<String>) -> Self {
        Self::GateRefused(message.into())
    }
    pub fn pre_write(message: impl Into<String>) -> Self {
        Self::PreWrite(message.into())
    }
    pub fn provider(message: impl Into<String>) -> Self {
        Self::Provider(message.into())
    }
    pub fn unknown(message: impl Into<String>) -> Self {
        Self::OutcomeUnknown(message.into())
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
    pub fn not_rendered(miss: RenderMiss) -> Self {
        Self::NotRendered(miss)
    }
    /// Invalid input with a stable code. The wire kind stays `rejected`.
    pub fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::Structured(Structured {
            kind: "rejected",
            code: code.to_string(),
            message: message.into(),
            revision: None,
        })
    }
    /// Revision mismatch. `revision` is the current stored revision.
    pub fn conflict(revision: i64, message: impl Into<String>) -> Self {
        Self::Structured(Structured {
            kind: "conflict",
            code: "revision_conflict".to_string(),
            message: message.into(),
            revision: Some(revision),
        })
    }
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Structured(structured) => Some(structured.code.as_str()),
            _ => None,
        }
    }
    pub fn revision(&self) -> Option<i64> {
        match self {
            Self::Structured(structured) => structured.revision,
            _ => None,
        }
    }
    /// Stable wire kind for [`crate::proto`].
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Rejected(_) => "rejected",
            Self::Provider(_) => "provider",
            Self::OutcomeUnknown(_) => "unknown",
            Self::Internal(_) => "internal",
            Self::GateRefused(_) => "gate",
            // Wire-compatible with `rejected`: it is one — the variant
            // only exists so the actor can match the pre-write proof.
            Self::PreWrite(_) => "rejected",
            // Internal to the actor loop — never a wire answer: the
            // daemon classifies it into requeue or `unknown` first.
            Self::NotRendered(_) => "not_rendered",
            Self::Structured(structured) => structured.kind,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(m)
            | Self::Provider(m)
            | Self::OutcomeUnknown(m)
            | Self::Internal(m)
            | Self::GateRefused(m)
            | Self::PreWrite(m) => f.write_str(m),
            Self::Structured(m) => f.write_str(&m.message),
            Self::NotRendered(m) => f.write_str(&m.reason),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Internal(format!("io: {e}"))
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Internal(format!("sqlite: {e}"))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Internal(format!("json: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
