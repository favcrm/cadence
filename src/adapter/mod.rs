//! Provider adapters behind one small trait.
//!
//! An adapter owns a provider connection (or a test double) for the life of
//! one agent actor. Turn outcomes distinguish `Provider` rejections from
//! `OutcomeUnknown` transport failures; the daemon preserves the latter for
//! review rather than replaying potentially executed work.

pub mod codex;
pub mod fake;
pub mod link;
pub mod pty;
pub mod stdio;
pub mod ws;

use serde_json::Value;

use crate::error::Result;
use crate::store::Agent;

/// Native identity returned by a successful `open`.
pub struct Identity {
    pub thread_id: String,
    pub session_id: String,
    pub model: Option<String>,
    pub pid: u32,
    /// Attachable endpoint (`ws://…`, `tmux://…`) when the kind has one.
    pub endpoint: Option<String>,
    /// Live endpoint generation minted per `open` (pty uses it for
    /// stale-report rejection); `None` where not applicable.
    pub generation: Option<String>,
}

/// A provider-initiated request (approval, user input). `id` is the raw
/// JSON-RPC request id needed to reply.
#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

/// Final turn outcome as reported by the provider.
pub struct TurnResult {
    pub turn_id: String,
    /// `completed`, `failed` or `interrupted`.
    pub status: String,
    pub text: String,
    pub stop_reason: Option<String>,
    pub error: Option<String>,
}

/// Notification sink: `(method, params)` for lifecycle events.
pub type EventHook = Box<dyn Fn(&str, Value) + Send + Sync>;
/// Request sink: provider-initiated requests needing a user decision.
pub type RequestHook = Box<dyn Fn(ProviderRequest) + Send + Sync>;

/// Callbacks the adapter uses to surface provider traffic.
pub struct AdapterHooks {
    /// Notification events worth recording (turn lifecycle, message items).
    pub on_event: EventHook,
    /// Provider-initiated requests that need a user decision.
    pub on_request: RequestHook,
}

pub trait ProviderAdapter: Send + Sync {
    /// Open or resume the provider session for `agent`.
    fn open(&self, agent: &Agent) -> Result<Identity>;
    /// Run one turn. `on_started` fires once the provider acknowledges a
    /// turn id; after that point a lost connection is `OutcomeUnknown`.
    fn run_turn(
        &self,
        prompt: &str,
        client_message_id: &str,
        on_started: &dyn Fn(&str),
    ) -> Result<TurnResult>;
    /// Answer a pending provider request (approval/user input).
    fn respond(&self, request_id: &Value, result: Value) -> Result<()>;
    /// Best-effort cancellation of an active turn.
    fn interrupt(&self);
    /// Transport is gone; any outstanding turn is ambiguous.
    fn disconnected(&self) -> bool;
    /// Release the provider process/connection and any owned resources.
    fn close(&self);
    /// Release control without killing user-visible resources — used on
    /// daemon shutdown. Defaults to `close`; pty overrides it so an
    /// owned tmux pane survives a controller restart (the operator may
    /// be looking at it) and is revalidated/reattached on next open.
    fn detach(&self) {
        self.close();
    }
    /// Record an operator readiness claim (pty-style gated endpoints
    /// only); the next send consumes it atomically.
    fn claim_ready(&self) -> Result<()> {
        Err(crate::error::Error::rejected(
            "this endpoint kind has no readiness gate",
        ))
    }
    /// Current terminal content for operator inspection (pty only).
    fn capture(&self) -> Result<String> {
        Err(crate::error::Error::rejected(
            "this endpoint kind has no capturable screen",
        ))
    }
}

/// Build the adapter for an agent's `provider`/`endpoint_kind`.
/// `endpoint_kind == "fake"` selects the in-process test double.
/// `log_path` receives provider stderr for managed adapters.
pub fn build(
    agent: &Agent,
    hooks: AdapterHooks,
    log_path: &std::path::Path,
) -> Result<Box<dyn ProviderAdapter>> {
    match agent.endpoint_kind.as_str() {
        "managed" => match agent.provider.as_str() {
            "codex" => Ok(Box::new(codex::CodexAdapter::new(hooks, log_path))),
            other => Err(crate::error::Error::rejected(format!(
                "No managed adapter for provider '{other}' (implemented: codex)"
            ))),
        },
        "managed-ws" => match agent.provider.as_str() {
            "codex" => Ok(Box::new(codex::CodexAdapter::new_ws(hooks, log_path))),
            other => Err(crate::error::Error::rejected(format!(
                "No managed-ws adapter for provider '{other}' (implemented: codex)"
            ))),
        },
        "pty" => match agent.provider.as_str() {
            "devin" => Ok(Box::new(pty::DevinPtyAdapter::new(hooks, log_path, agent)?)),
            other => Err(crate::error::Error::rejected(format!(
                "No pty adapter for provider '{other}' (implemented: devin)"
            ))),
        },
        "fake" => Ok(Box::new(fake::FakeAdapter::new(hooks))),
        other => Err(crate::error::Error::rejected(format!(
            "Endpoint kind '{other}' is not implemented \
             (implemented: managed, managed-ws, pty, fake)"
        ))),
    }
}
