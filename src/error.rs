//! Error taxonomy, mirroring the reference implementation.
//!
//! - [`Error::Rejected`]: the caller made an invalid or disallowed request.
//! - [`Error::Provider`]: the provider explicitly rejected a request.
//! - [`Error::OutcomeUnknown`]: the connection failed after a request could
//!   have reached the provider. The outcome must be preserved for review,
//!   never silently retried.
//! - [`Error::Internal`]: local runtime failures (I/O, storage, protocol).
//! - [`Error::busy`]: transient resource contention, not loss of authority.
//!   Only a condition that frees by itself may be `busy`; a standing
//!   refusal that needs an operator is a coded `gate` ([`Error::gate_coded`]).

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
/// input, `conflict` when a caller must reload rather than retry, and `busy`
/// when an operation could not acquire a transiently occupied resource.
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
    /// CAD-1142: an actor-fatal error raised on an app-owned turn — the
    /// actor's own record of which turn died (`source ==
    /// "app_run_dispatch"` at claim), never a caller claim or an
    /// error-text guess. `run_actor`'s fatal arm wraps the returned error
    /// so the public fence text (`agents.error`, the `attention` event)
    /// can carry a bounded Cadence-authored class; `Display` still
    /// returns the raw account, and `kind` stays `provider`, so the
    /// private detail and the wire shape are unchanged. The raw account
    /// stays on the message row (operator-gated).
    AppOwnedFatal(String),
    OutcomeUnknown(String),
    Internal(String),
    /// CAD-1283: a spawned `ui run` proved its own bind failed with
    /// `AddrInUse` — the `ui.startup-failed` frame matched this start's
    /// child pid and readiness nonce, so the classification is typed
    /// evidence, never a log substring. Only `dev up`'s fresh automatic
    /// allocation may retry it; every other caller sees an ordinary
    /// failure (exit 1 via the default table).
    UiBindInUse(String),
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
    /// Boxed: the evidence (two screen tails + probe verdicts) is far
    /// bigger than the other variants' payloads.
    NotRendered(Box<RenderMiss>),
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
    /// CAD-1142 reason privacy: mark an actor-fatal error as raised on
    /// an app-owned turn. Only `run_actor`'s fatal arm calls this, with
    /// the claimed message's proven source — never derived from the
    /// error text. All other variants pass through unchanged.
    pub fn into_app_owned_fatal(self) -> Self {
        match self {
            // Already stamped, or a variant the actor never reaches the
            // fatal arm with (outcome-unknown, gate, pre-write,
            // render-miss are intercepted earlier): keep it as-is.
            Self::AppOwnedFatal(_)
            | Self::OutcomeUnknown(_)
            | Self::GateRefused(_)
            | Self::PreWrite(_)
            | Self::NotRendered(_) => self,
            other => Self::AppOwnedFatal(other.to_string()),
        }
    }
    /// True when this fatal error was provably raised on an app-owned
    /// turn — the actor-stamped marker, not a text heuristic.
    pub fn is_app_owned_fatal(&self) -> bool {
        matches!(self, Self::AppOwnedFatal(_))
    }
    pub fn unknown(message: impl Into<String>) -> Self {
        Self::OutcomeUnknown(message.into())
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
    /// CAD-1283: [`Error::UiBindInUse`].
    pub fn ui_bind_in_use(message: impl Into<String>) -> Self {
        Self::UiBindInUse(message.into())
    }
    pub fn not_rendered(miss: RenderMiss) -> Self {
        Self::NotRendered(Box::new(miss))
    }
    /// Resource contention is not proof that a request or its authority is
    /// invalid. The operation still fails closed; callers may retry with their
    /// original idempotency key, never assume a side effect was undone.
    pub fn busy(message: impl Into<String>) -> Self {
        Self::Structured(Structured {
            kind: "busy",
            code: "resource_busy".to_string(),
            message: message.into(),
            revision: None,
        })
    }
    /// Resource contention with a stable code — still `busy` (75).
    /// `waking` names the remote-org wake budget specifically.
    pub fn busy_coded(code: &'static str, message: impl Into<String>) -> Self {
        Self::Structured(Structured {
            kind: "busy",
            code: code.to_string(),
            message: message.into(),
            revision: None,
        })
    }
    /// A standing refusal with a stable code: no retry helps until an
    /// operator clears the reason (exit 4, never the retryable `busy`).
    pub fn gate_coded(code: &'static str, message: impl Into<String>) -> Self {
        Self::Structured(Structured {
            kind: "gate",
            code: code.to_string(),
            message: message.into(),
            revision: None,
        })
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
    /// A command line the parser rejected (CAD-876). Printed by `main`
    /// like any other error, so usage errors share the JSON shape.
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Structured(Structured {
            kind: "usage",
            code: "usage".to_string(),
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
            // Wire-compatible with `provider`: it is one — the variant
            // only exists so `run_actor` can classify the public fence
            // text for an app-owned turn without reading the prose.
            Self::Provider(_) | Self::AppOwnedFatal(_) => "provider",
            Self::OutcomeUnknown(_) => "unknown",
            Self::Internal(_) => "internal",
            Self::UiBindInUse(_) => "ui_bind_in_use",
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

/// The one table from an error `kind` to the CLI process exit code
/// (CAD-876). `main` and nothing else maps kinds to codes, so an agent
/// can branch on `$?` without parsing stderr. Codes 0 and 1 and the
/// verb-specific 2 (`doctor --host`, `setup`, `audit`, `message`
/// pending, `agent-uid provision`) predate the table; it only adds
/// codes that none of them uses, and `usage` shares the clap-compatible
/// 2. Only `busy` (75) may be retried blindly, a bounded few times;
/// `unknown` (1) may be retried only after checking whether the effect
/// landed. `gate` (4) is final until an operator clears its reason. An
/// unrecognised kind exits 1, like the old generic failure.
pub const EXIT_TABLE: &[(&str, i32)] = &[
    ("usage", 2),
    ("rejected", 3),
    ("gate", 4),
    ("conflict", 5),
    ("provider", 6),
    ("internal", 70),
    ("busy", 75),
    ("unknown", 1),
];

/// Exit code for an error `kind`; anything not in [`EXIT_TABLE`] is 1.
pub fn exit_code_for_kind(kind: &str) -> i32 {
    EXIT_TABLE
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(1, |(_, code)| *code)
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(m)
            | Self::Provider(m)
            | Self::AppOwnedFatal(m)
            | Self::OutcomeUnknown(m)
            | Self::Internal(m)
            | Self::UiBindInUse(m)
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
