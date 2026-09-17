//! Error taxonomy, mirroring the reference implementation.
//!
//! - [`Error::Rejected`]: the caller made an invalid or disallowed request.
//! - [`Error::Provider`]: the provider explicitly rejected a request.
//! - [`Error::OutcomeUnknown`]: the connection failed after a request could
//!   have reached the provider. The outcome must be preserved for review,
//!   never silently retried.
//! - [`Error::Internal`]: local runtime failures (I/O, storage, protocol).

use std::fmt;

#[derive(Debug)]
pub enum Error {
    Rejected(String),
    Provider(String),
    OutcomeUnknown(String),
    Internal(String),
    /// The endpoint is not safe to submit to right now; the message
    /// returns to `queued` and is retried, never failed or pasted blind.
    GateRefused(String),
    /// Deterministic rejection made *before* any bytes could reach the
    /// provider — the message fails, but the endpoint is provably
    /// untouched, so the actor must not fence or close it.
    PreWrite(String),
    /// The paste was accepted by the terminal path but never rendered —
    /// provably not delivered (pty post-paste screen check). The actor
    /// decides: routed notifications requeue, task messages go
    /// `unknown` under the usual uncertainty discipline.
    NotRendered(String),
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
    pub fn not_rendered(message: impl Into<String>) -> Self {
        Self::NotRendered(message.into())
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
            | Self::PreWrite(m)
            | Self::NotRendered(m) => f.write_str(m),
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
