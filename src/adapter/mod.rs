//! Provider adapters behind one small trait.
//!
//! An adapter owns a provider connection (or a test double) for the life of
//! one agent actor. Turn outcomes distinguish `Provider` rejections from
//! `OutcomeUnknown` transport failures; the daemon preserves the latter for
//! review rather than replaying potentially executed work.

pub mod claude;
pub mod cloud;

/// Environment names a spawned worker must not inherit. Devin cloud
/// credentials stay on the daemon.
pub const CLOUD_SECRET_ENV: &[&str] = &[
    "DEVIN_API_KEY",
    "DEVIN_ORG_ID",
    "CADENCE_DEVIN_API_KEY",
    "CADENCE_DEVIN_ORG_ID",
    "CADENCE_DEVIN_API_BASE",
    "CADENCE_DEVIN_POLL_INTERVAL_MS",
    "CADENCE_DEVIN_POLL_BUDGET_MS",
    "CADENCE_DEVIN_RECOVER_BUDGET_MS",
];

/// `env -u` arguments that drop [`CLOUD_SECRET_ENV`] before a pane command.
pub fn cloud_secret_env_prefix() -> String {
    CLOUD_SECRET_ENV
        .iter()
        .map(|name| format!("-u {name}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The daemon's own tracker and profile. Every agent it launches must
/// share them, so a sandbox worker's `cadence issue …` reaches the
/// sandbox's tracker, gated (CAD-310). Provider scrubs that drop
/// `CADENCE_*` re-inject exactly these beside the agent's alias and
/// state dir.
pub const DAEMON_CONTEXT_ENV: [&str; 2] = ["CADENCE_PM_DIR", "CADENCE_PROFILE"];

/// [`DAEMON_CONTEXT_ENV`] as this daemon has it; empty values are
/// skipped.
pub fn daemon_context_env(env: &ProviderEnv) -> Vec<(String, String)> {
    DAEMON_CONTEXT_ENV
        .iter()
        .filter_map(|name| {
            env.var(name)
                .filter(|v| !v.is_empty())
                .map(|v| (name.to_string(), v))
        })
        .collect()
}
pub mod codex;
pub mod fake;
pub mod link;
pub mod pty;
pub mod registry;
pub mod stdio;
pub mod ws;

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde_json::Value;

use crate::error::{Error, Result};
use crate::store::Agent;

/// Per-daemon values for the provider launch variables
/// (`CADENCE_CLAUDE_COMMAND`, `CADENCE_TMUX_COMMAND`, …). A name set
/// here wins; an unset one falls back to the process environment,
/// which stays the operator-facing override. In-process test daemons
/// set their mocks here — the environment is shared by every daemon
/// in the process, so a mock installed there reaches all of them.
#[derive(Clone, Default)]
pub struct ProviderEnv(Arc<RwLock<BTreeMap<String, String>>>);

impl ProviderEnv {
    pub fn set(&self, name: &str, value: impl Into<String>) {
        self.0
            .write()
            .unwrap()
            .insert(name.to_string(), value.into());
    }

    pub fn remove(&self, name: &str) {
        self.0.write().unwrap().remove(name);
    }

    /// Every value set here — for handing to a daemon this one spawns
    /// as a separate process (`daemon restart`), which sees only env.
    pub fn vars(&self) -> Vec<(String, String)> {
        self.0
            .read()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Value set on this daemon only. Does not consult the process
    /// environment, so a blank override cannot fall through to a real
    /// credential that happens to be exported.
    pub fn own(&self, name: &str) -> Option<String> {
        self.0.read().unwrap().get(name).cloned()
    }

    /// This daemon's value for `name`, else the environment's.
    pub fn var(&self, name: &str) -> Option<String> {
        let own = self.0.read().unwrap().get(name).cloned();
        own.or_else(|| std::env::var(name).ok())
    }
}

/// Native identity returned by a successful `open`.
pub struct Identity {
    pub thread_id: String,
    pub session_id: String,
    pub model: Option<String>,
    /// Provider-confirmed reasoning effort for the opened native thread,
    /// when the endpoint reports one (Codex app-server does).
    pub effort: Option<String>,
    pub pid: u32,
    /// Attachable endpoint (`ws://…`, `tmux://…`) when the kind has one.
    pub endpoint: Option<String>,
    /// Live endpoint generation minted per `open` (pty uses it for
    /// stale-report rejection); `None` where not applicable.
    pub generation: Option<String>,
    /// How the endpoint came up for attachable surfaces: `"adopted"`
    /// when an existing pane was re-attached (same pid and native
    /// session), `"respawned"` when a new pane was launched on the
    /// recorded session. `None` for kinds with no such distinction.
    pub attach: Option<&'static str>,
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

/// What [`ProviderAdapter::interrupt_turn`] did (CAD-323).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptOutcome {
    /// The provider's own interrupt reached the running turn; the turn's
    /// result arrives on the wire and finishes the message as usual.
    Delivered,
    /// The interrupt keys reached a terminal that has no result wire
    /// (pty): nothing will report this turn, so the caller records the
    /// `interrupted` finish itself.
    Unsettled,
    /// The named turn is not the one in flight (it already ended) —
    /// nothing was sent.
    NotRunning,
}

/// Outcome of one held-recovery poll.
pub enum SettledPoll {
    /// The remote session reached a terminal outcome for this turn.
    Ready(TurnResult),
    /// Still working, or the poll could not be learned. `transient` is a
    /// 429, 5xx, or transport error; the caller backs off.
    Pending { transient: bool },
}

/// Screen-probe verdict for terminal endpoints: what the pane shows
/// right now, reduced to the facts the gate needs. `reason` names the
/// first condition that makes the pane not-idle (or "idle").
#[derive(Debug, Clone)]
pub struct Probe {
    pub idle: bool,
    pub reason: String,
    /// Text staged in the input line (typed but not submitted).
    pub input_nonempty: bool,
    /// An empty `❭`-style prompt line is visible.
    pub prompt_visible: bool,
    /// A working/spinner marker is on screen.
    pub busy_marker: bool,
    /// A provider approval/permission menu is on screen.
    pub approval_menu: bool,
}

impl Probe {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "idle": self.idle,
            "reason": self.reason,
            "input_nonempty": self.input_nonempty,
            "prompt_visible": self.prompt_visible,
            "busy_marker": self.busy_marker,
            "approval_menu": self.approval_menu,
        })
    }
}

/// Notification sink: `(method, params)` for lifecycle events. Methods
/// prefixed `cadence/` are recorded verbatim as event kinds — the
/// adapter's own bookkeeping channel, not provider traffic.
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
    /// Re-open after a provably clean daemon restart to adopt the
    /// recorded in-flight turn (CAD-89): the endpoint must still be
    /// the one the shutdown recorded — same pane pid, same native
    /// session — and the recorded endpoint generation is reused so
    /// the turn's token stays valid. Managed kinds cannot adopt: a
    /// provider process dies with its daemon, so the default refuses.
    fn open_adopted(
        &self,
        _agent: &Agent,
        _adoption: &crate::store::AdoptEntry,
    ) -> Result<Identity> {
        Err(crate::error::Error::rejected(
            "this endpoint kind cannot adopt turns",
        ))
    }
    /// Run one turn. `on_started` fires once the provider acknowledges a
    /// turn id; after that point a lost connection is `OutcomeUnknown`.
    fn run_turn(
        &self,
        prompt: &str,
        client_message_id: &str,
        on_started: &dyn Fn(&str),
    ) -> Result<TurnResult>;
    /// One poll after a held cloud turn. `Pending` means keep waiting.
    /// `transient` is a 429, 5xx, or transport error. The default never
    /// settles.
    fn poll_settled(&self) -> Result<SettledPoll> {
        Ok(SettledPoll::Pending { transient: false })
    }
    /// Minimum gap between held-recovery polls. Cloud uses its poll interval.
    fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }
    /// How long held-recovery may keep polling before it escalates once
    /// and stops. Cloud reads `CADENCE_DEVIN_RECOVER_BUDGET_MS`.
    fn recover_budget(&self) -> std::time::Duration {
        std::time::Duration::from_secs(30 * 60)
    }
    /// Answer a pending provider request (approval/user input).
    fn respond(&self, request_id: &Value, result: Value) -> Result<()>;
    /// Best-effort cancellation of an active turn.
    fn interrupt(&self);
    /// `cadence interrupt` (CAD-323): stop exactly the running turn
    /// `turn_id` with the provider's own interrupt — never a kill. A
    /// turn that is no longer in flight is [`InterruptOutcome::NotRunning`]
    /// and nothing is sent. The default refuses: an endpoint without a
    /// provider-native, non-destructive interrupt (Devin cloud's
    /// `interrupt` terminates the session) must not pretend to have one.
    fn interrupt_turn(&self, turn_id: &str) -> Result<InterruptOutcome> {
        let _ = turn_id;
        Err(Error::rejected(
            "this endpoint has no provider-native turn interrupt — \
             `cadence agent stop` ends the endpoint instead",
        ))
    }
    /// What `agent stop` does before it waits. Defaults to [`interrupt`].
    /// Devin cloud overrides this: `interrupt` terminates the remote
    /// session, while stop must archive it via [`close`] instead.
    fn release_for_stop(&self) {
        self.interrupt();
    }
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
    /// only); `by` names the claimer (the caller's `CADENCE_ALIAS` when
    /// set) for the audit record. Claims stack FIFO — each is consumed
    /// by exactly one send. The claim runs the same screen probe the
    /// verified auto-ready gate uses and refuses a visibly busy pane —
    /// `force` overrides the refusal (recorded on the claim). Returns
    /// the probe verdict the claim was admitted under.
    fn claim_ready(&self, _by: Option<String>, _force: bool) -> Result<Probe> {
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
    /// Inspect the screen and reduce it to gate facts (pty only).
    fn probe(&self) -> Result<Probe> {
        Err(crate::error::Error::rejected(
            "this endpoint kind has no screen probe",
        ))
    }
    /// Read-only proof that this adapter still owns the same native endpoint
    /// recorded by the store. Memory identity uses this instead of treating
    /// a live pid and metadata row as sufficient ownership.
    fn verify_owned_endpoint(
        &self,
        _expected_pid: u32,
        _expected_generation: &str,
        _expected_native: Option<&str>,
    ) -> Result<()> {
        Err(crate::error::Error::rejected(
            "endpoint kind has no native ownership proof",
        ))
    }
    /// `agent answer`: send one menu-choice keystroke to a pane that
    /// currently probes `approval_menu` (pty only). The adapter
    /// re-probes first and refuses anything that isn't a live menu —
    /// the key can never land in a prompt or a running turn. `choice`
    /// is the option's printed index; the profile's own keymap maps it
    /// to keystrokes. Returns the probe the answer was admitted under
    /// (the menu line rides on `reason`).
    fn answer_approval(&self, _choice: &str) -> Result<Probe> {
        Err(crate::error::Error::rejected(
            "this endpoint kind has no approval-menu channel",
        ))
    }
    /// One bounded liveness sample for the stall watch: the activity
    /// hash and the probe verdict from a single screen capture (pty
    /// only). A menu or an idle frame both carry information a plain
    /// hash cannot — the hash alone cannot tell "ended at the prompt"
    /// from "busy churning".
    fn sample_screen(&self) -> Result<(String, Probe)> {
        Err(crate::error::Error::rejected(
            "this endpoint kind has no screen sample",
        ))
    }
    /// A merged params patch landed in the store — live adapters that
    /// cache endpoint options (pty's `auto_ready`) refresh here. Stored
    /// params remain authoritative for the next `open` regardless.
    fn update_params(&self, _params: &Value) {}
    /// The actor is about to run one turn. `ok` is true only for a
    /// routed notice (`worker_result`, `worker_notice`, `job_event`):
    /// the pty gate may then paste into an idle pane with no operator
    /// claim. Cleared when the turn returns so a later user message
    /// cannot inherit it. No-op except on the pty adapter — the flag
    /// is not encoded in the message body.
    fn set_unclaimed_ok(&self, _ok: bool) {}
    /// External proof of life: a brokered permission request is open
    /// and waiting on a human — the provider is silent by design, so
    /// activity-based liveness must count the wait or the turn fences
    /// `unknown` while the operator thinks. No-op where liveness isn't
    /// activity-derived (pty watches its pane instead).
    fn note_activity(&self) {}
    /// The adapter's own last-observed provider activity, when it keeps
    /// a raw clock (managed transcripts stamp every notification).
    /// `None` means the daemon's event clock is the activity source —
    /// pty agents are additionally sampled by screen hash.
    fn activity_at(&self) -> Option<std::time::Instant> {
        None
    }
    /// Provider-owned allowance telemetry captured during `open`, when this
    /// adapter has a bounded source for it. The returned object is an
    /// adapter snapshot only; the store binds provider, assignee, thread,
    /// and timestamps before exposing it as `agent.quota`.
    fn quota_snapshot(&self) -> Option<Value> {
        None
    }
}

/// Build the adapter for an agent's `provider`/`endpoint_kind`.
/// `endpoint_kind == "fake"` selects the in-process test double.
/// `log_path` receives provider stderr for managed adapters.
pub fn build(
    agent: &Agent,
    hooks: AdapterHooks,
    log_path: &std::path::Path,
    env: &ProviderEnv,
) -> Result<Box<dyn ProviderAdapter>> {
    // The registry is consulted for pair validation only — its
    // rejections carry the same per-kind text the match arms below
    // produced inline. `fake` resolves for any provider via the
    // registry's provider-agnostic lookup, so no bypass is needed here.
    registry::spec(&agent.provider, &agent.endpoint_kind)?;
    match agent.endpoint_kind.as_str() {
        "managed" => match agent.provider.as_str() {
            "codex" => Ok(Box::new(codex::CodexAdapter::new(hooks, log_path, env))),
            "claude" => Ok(Box::new(claude::ClaudeAdapter::new(hooks, log_path, env))),
            other => Err(crate::error::Error::rejected(format!(
                "No managed adapter for provider '{other}' (implemented: codex, claude)"
            ))),
        },
        "managed-ws" => match agent.provider.as_str() {
            "codex" => Ok(Box::new(codex::CodexAdapter::new_ws(hooks, log_path, env))),
            other => Err(crate::error::Error::rejected(format!(
                "No managed-ws adapter for provider '{other}' (implemented: codex)"
            ))),
        },
        "cloud" => match agent.provider.as_str() {
            "devin" => Ok(Box::new(cloud::DevinCloudAdapter::new(hooks, env))),
            other => Err(crate::error::Error::rejected(format!(
                "No cloud adapter for provider '{other}' (implemented: devin)"
            ))),
        },
        "pty" => match agent.provider.as_str() {
            "devin" => Ok(Box::new(pty::PtyAdapter::new(
                hooks,
                log_path,
                agent,
                env,
                // The stored permission mode rides the profile so the
                // same argv is replayed on every open — fresh launch
                // and `-r` resume alike.
                pty::DevinProfile::new(env)?.with_permission_mode(agent.params.as_ref().and_then(
                    |p| {
                        p.get("permission_mode")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    },
                )),
            )?)),
            "claude" => Ok(Box::new(pty::PtyAdapter::new(
                hooks,
                log_path,
                agent,
                env,
                pty::ClaudeProfile::new(agent, env)?,
            )?)),
            "cursor" => Ok(Box::new(pty::PtyAdapter::new(
                hooks,
                log_path,
                agent,
                env,
                pty::CursorProfile::new(agent, env)?,
            )?)),
            // Harness double: proves the adapter is profile-driven.
            "tui-stub" => Ok(Box::new(pty::PtyAdapter::new(
                hooks,
                log_path,
                agent,
                env,
                pty::StubProfile::new(env)?,
            )?)),
            other => Err(crate::error::Error::rejected(format!(
                "No pty adapter for provider '{other}' (implemented: devin, claude, cursor)"
            ))),
        },
        "fake" => Ok(Box::new(fake::FakeAdapter::new(hooks))),
        other => Err(crate::error::Error::rejected(format!(
            "Endpoint kind '{other}' is not implemented \
             (implemented: managed, managed-ws, pty, cloud, fake)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn assert_child_lacks_secrets(command: &mut Command) {
        let output = command.output().expect("spawn env");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            !text.contains("cog_test_secret_value"),
            "spawned env contained a devin cloud secret:\n{text}"
        );
        for name in CLOUD_SECRET_ENV {
            assert!(
                !text
                    .lines()
                    .any(|line| line.starts_with(&format!("{name}="))),
                "{name} leaked into a spawned worker:\n{text}"
            );
        }
    }

    #[test]
    fn spawned_worker_env_drops_devin_cloud_secrets() {
        let mut pane = Command::new("env");
        for name in CLOUD_SECRET_ENV {
            pane.env(name, "cog_test_secret_value");
        }
        pane.args(cloud_secret_env_prefix().split_whitespace());
        assert_child_lacks_secrets(&mut pane);

        let claude = claude::claude_env_scrub();
        let mut managed = Command::new("env");
        for name in CLOUD_SECRET_ENV {
            assert!(claude.removes_name(name), "claude scrub missed {name}");
            managed.env(name, "cog_test_secret_value");
            managed.env_remove(name);
        }
        assert_child_lacks_secrets(&mut managed);

        let codex = codex::scrub_names();
        let mut app = Command::new("env");
        for name in CLOUD_SECRET_ENV {
            assert!(codex.contains(name), "codex scrub missed {name}");
            app.env(name, "cog_test_secret_value");
            app.env_remove(name);
        }
        assert_child_lacks_secrets(&mut app);
    }

    /// CAD-310: the tracker and profile a launched agent gets back are
    /// the daemon's own values; an empty one is not re-injected.
    #[test]
    fn daemon_context_env_is_the_daemon_tracker_and_profile() {
        let env = ProviderEnv::default();
        env.set("CADENCE_PM_DIR", "/sbx/pm");
        env.set("CADENCE_PROFILE", "sandbox:x");
        assert_eq!(
            daemon_context_env(&env),
            vec![
                ("CADENCE_PM_DIR".to_string(), "/sbx/pm".to_string()),
                ("CADENCE_PROFILE".to_string(), "sandbox:x".to_string()),
            ]
        );
        env.set("CADENCE_PROFILE", "");
        assert_eq!(
            daemon_context_env(&env),
            vec![("CADENCE_PM_DIR".to_string(), "/sbx/pm".to_string())]
        );
    }
}
