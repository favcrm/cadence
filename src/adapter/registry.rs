//! Capability registry: one descriptor per `(provider, endpoint_kind)`
//! pair, consulted by the daemon, CLI, doctor and board instead of
//! scattered string comparisons.
//!
//! The table is the single place that knows what an endpoint can do —
//! actor ownership, attach surface, ready gate, reporting style, request
//! brokering, param surface and provider CLI probes. `build` keeps
//! constructing adapters; [`spec`] validates the pair first and its
//! rejections reproduce the factory's historical error text exactly.
//!
//! This module deliberately knows nothing about adapter construction —
//! the pty split keeps it that way.

use std::sync::LazyLock;

use serde_json::{json, Value};

use crate::error::{Error, Result};

/// The mailbox provider and endpoint kind share one name.
pub const INBOX: &str = "inbox";

/// `endpoint_kind` when `agent_register` callers omit it — the managed
/// default, kept verbatim from the historical `unwrap_or`.
pub const DEFAULT_ENDPOINT_KIND: &str = "managed";

/// The four modes `devin --permission-mode` accepts. `--bypass` is a
/// launch shorthand that stores `dangerous`; it is never a stored
/// value itself.
pub const DEVIN_PERMISSION_MODES: &[&str] = &["auto", "accept-edits", "smart", "dangerous"];

/// The modes `cursor --permission-mode` accepts — each maps to one
/// `cursor-agent` flag (`--auto-review`, `--force`). `--bypass` is a
/// launch shorthand that stores `force`; it is never a stored value
/// itself.
pub const CURSOR_PERMISSION_MODES: &[&str] = &["auto-review", "force"];

/// How a live endpoint's native surface is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attach {
    /// Nothing attachable — no terminal surface exists.
    None,
    /// Headless provider process (managed stream-json): nothing to
    /// attach, but `agent attach` explains how to observe and drive it
    /// instead of issuing a bare rejection.
    Headless,
    /// cadence-owned tmux pane: `tmux -L <socket> attach-session`.
    Tmux,
    /// Provider TUI attach; the payload is the program to exec
    /// (`codex resume --remote <endpoint> <thread>`).
    ProviderTui(&'static str),
}

/// How a durable message on this endpoint gets completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reporting {
    /// The worker (or operator) reports via `cadence message result`.
    Explicit,
    /// The adapter's turn result completes the message — no token flow.
    TurnResult,
}

/// One `(provider, endpoint_kind)` capability descriptor.
#[derive(Debug)]
pub struct EndpointSpec {
    pub provider: &'static str,
    pub endpoint_kind: &'static str,
    /// Human-readable name for listings.
    pub display: &'static str,
    /// False for the mailbox — durable queue, no actor or process.
    pub has_actor: bool,
    pub attach: Attach,
    /// Operator ready-claim gate before pty submission.
    pub ready_gate: bool,
    /// Screen capture/probe exists (pty only).
    pub screen_probe: bool,
    /// Transport fact: does the adapter's turn result complete messages.
    pub reports: Reporting,
    /// Briefing fact: which report instruction the briefing prints. Not
    /// always equal to `reports` — managed-ws workers are humans in a
    /// TUI and are still taught the explicit token flow.
    pub report_hint: Reporting,
    /// Provider-initiated JSON-RPC requests reach `agent respond`.
    pub brokers_requests: bool,
    /// `agent resume` / launch `-r` is meaningful for this endpoint.
    pub resumable: bool,
    /// What resume means, for display (`codex resume --remote`, …).
    pub resume_label: &'static str,
    /// Params `agent set` may patch live.
    pub live_settable_params: &'static [&'static str],
    /// Params the launch verbs accept into `params` (validated by the
    /// verb's own flags; this list is the rendered surface).
    pub launch_params: &'static [&'static str],
    /// Label for the native session id in messages ("Codex thread").
    pub session_id_label: &'static str,
    /// Provider-specific `agent respond` rejection — `Some` short-circuits
    /// before the pending-request map is consulted.
    pub respond_rejection: Option<&'static str>,
    /// Capability names this spec contributes to `health.capabilities`.
    pub capabilities: &'static [&'static str],
    /// Keys this spec contributes to `doctor.capabilities`, each gated on
    /// every `probe_bins` binary being present.
    pub doctor_caps: &'static [&'static str],
    /// Provider binaries `doctor` probes: `(program, version-args)`.
    pub probe_bins: &'static [(&'static str, &'static [&'static str])],
    /// Whether a stored native session id may be discarded when a
    /// resume proves it can never come back — the daemon-side gate for
    /// `cadence/session_resume_failed`, paired with the profile's
    /// `session_is_disposable`. True only for cursor chats: cheap
    /// mintable ids with no operator-chosen meaning.
    pub session_disposable: bool,
    /// This pair is the provider's launch-verb endpoint kind
    /// (`cadence codex` → managed-ws, never managed stdio).
    pub launch_default: bool,
    /// Harness/test-double pair: builds through the factory but is not
    /// user-facing — excluded from `spec`'s "(implemented: …)" listings
    /// so its error text matches the factory arms verbatim.
    pub internal: bool,
}

/// The registry. Order is deliberate: capabilities and doctor output
/// render in this order.
pub static SPECS: &[EndpointSpec] = &[
    EndpointSpec {
        provider: "codex",
        endpoint_kind: "managed",
        display: "Codex (managed stdio)",
        has_actor: true,
        attach: Attach::None,
        ready_gate: false,
        screen_probe: false,
        reports: Reporting::TurnResult,
        report_hint: Reporting::Explicit,
        brokers_requests: true,
        resumable: true,
        resume_label: "codex resume --remote",
        live_settable_params: &["stall_secs"],
        launch_params: &[
            "model",
            "effort",
            "session",
            "upstream",
            "agents_md",
            "stall_secs",
            "approval_policy",
        ],
        session_id_label: "Codex thread",
        respond_rejection: None,
        capabilities: &["managed_codex_stdio"],
        doctor_caps: &["managed_codex_stdio"],
        probe_bins: &[("codex", &["--version"])],
        session_disposable: false,
        launch_default: false,
        internal: false,
    },
    EndpointSpec {
        provider: "codex",
        endpoint_kind: "managed-ws",
        display: "Codex (managed-ws app-server)",
        has_actor: true,
        attach: Attach::ProviderTui("codex"),
        ready_gate: false,
        screen_probe: false,
        reports: Reporting::TurnResult,
        report_hint: Reporting::Explicit,
        brokers_requests: true,
        resumable: true,
        resume_label: "codex resume --remote",
        live_settable_params: &["stall_secs"],
        launch_params: &[
            "model",
            "effort",
            "session",
            "upstream",
            "agents_md",
            "stall_secs",
            "approval_policy",
        ],
        session_id_label: "Codex thread",
        respond_rejection: None,
        capabilities: &["managed_codex_ws"],
        doctor_caps: &["managed_codex_ws"],
        probe_bins: &[("codex", &["--version"])],
        session_disposable: false,
        launch_default: true,
        internal: false,
    },
    EndpointSpec {
        provider: "claude",
        endpoint_kind: "managed",
        display: "Claude (managed stream-json)",
        has_actor: true,
        attach: Attach::Headless,
        ready_gate: false,
        screen_probe: false,
        reports: Reporting::TurnResult,
        report_hint: Reporting::TurnResult,
        brokers_requests: true,
        resumable: true,
        resume_label: "claude --resume <session>",
        live_settable_params: &["stall_secs"],
        launch_params: &[
            "model",
            "effort",
            "permission_mode",
            "allowed_tools",
            "turn_idle_secs",
            "turn_max_secs",
            "broker_approvals",
            "permission_timeout_secs",
            "session",
            "upstream",
            "agents_md",
            "stall_secs",
        ],
        session_id_label: "Claude session",
        respond_rejection: Some(
            "no request is pending for this managed claude endpoint — \
             permission prompts are brokered only when launched with \
             `--broker-approvals`; otherwise widen permissions by \
             relaunching or rejoining with `--permission-mode <mode>` \
             or `--allow \"<pattern>\"` (or `--bypass`); denials are \
             recorded as permission_denied events on the agent",
        ),
        capabilities: &["managed_claude_stream"],
        doctor_caps: &["managed_claude_stream"],
        probe_bins: &[("claude", &["--version"])],
        session_disposable: false,
        launch_default: true,
        internal: false,
    },
    EndpointSpec {
        provider: "claude",
        endpoint_kind: "pty",
        display: "Claude (pty/tmux)",
        has_actor: true,
        attach: Attach::Tmux,
        ready_gate: true,
        screen_probe: true,
        reports: Reporting::Explicit,
        report_hint: Reporting::Explicit,
        brokers_requests: false,
        resumable: true,
        resume_label: "claude --resume <session>",
        live_settable_params: &["auto_ready", "stall_secs", "silent_end_secs"],
        launch_params: &[
            "model",
            "effort",
            "permission_mode",
            "bypass",
            "allowed_tools",
            "session",
            "upstream",
            "auto_ready",
            "silent_end_secs",
            "agents_md",
            "stall_secs",
        ],
        session_id_label: "Claude session",
        respond_rejection: None,
        capabilities: &["pty_claude_tmux", "pty_verified_autoready"],
        doctor_caps: &["pty_claude_tmux"],
        probe_bins: &[("claude", &["--version"]), ("tmux", &["-V"])],
        session_disposable: false,
        launch_default: false,
        internal: false,
    },
    EndpointSpec {
        provider: "devin",
        endpoint_kind: "pty",
        display: "Devin (pty/tmux)",
        has_actor: true,
        attach: Attach::Tmux,
        ready_gate: true,
        screen_probe: true,
        reports: Reporting::Explicit,
        report_hint: Reporting::Explicit,
        brokers_requests: false,
        resumable: true,
        resume_label: "devin -r <slug>",
        live_settable_params: &["auto_ready", "stall_secs", "silent_end_secs"],
        launch_params: &[
            "session",
            "upstream",
            "auto_ready",
            "silent_end_secs",
            "permission_mode",
            "bypass",
            "agents_md",
            "stall_secs",
        ],
        session_id_label: "Devin session",
        respond_rejection: None,
        capabilities: &["pty_devin_tmux", "pty_verified_autoready"],
        doctor_caps: &["pty_devin_tmux"],
        probe_bins: &[("devin", &["--version"]), ("tmux", &["-V"])],
        session_disposable: false,
        launch_default: true,
        internal: false,
    },
    EndpointSpec {
        provider: "cursor",
        endpoint_kind: "pty",
        display: "Cursor (pty/tmux)",
        has_actor: true,
        attach: Attach::Tmux,
        ready_gate: true,
        screen_probe: true,
        reports: Reporting::Explicit,
        report_hint: Reporting::Explicit,
        brokers_requests: false,
        resumable: true,
        resume_label: "cursor-agent --resume <session>",
        live_settable_params: &["auto_ready", "stall_secs", "silent_end_secs"],
        launch_params: &[
            "model",
            "permission_mode",
            "bypass",
            "session",
            "upstream",
            "auto_ready",
            "silent_end_secs",
            "agents_md",
            "stall_secs",
        ],
        session_id_label: "Cursor chat",
        respond_rejection: None,
        capabilities: &["pty_cursor_tmux", "pty_verified_autoready"],
        doctor_caps: &["pty_cursor_tmux"],
        probe_bins: &[("cursor-agent", &["--version"]), ("tmux", &["-V"])],
        session_disposable: true,
        launch_default: true,
        internal: false,
    },
    EndpointSpec {
        provider: "tui-stub",
        endpoint_kind: "pty",
        display: "Stub TUI (pty profile double)",
        has_actor: true,
        attach: Attach::Tmux,
        ready_gate: true,
        screen_probe: true,
        reports: Reporting::Explicit,
        report_hint: Reporting::Explicit,
        brokers_requests: false,
        resumable: true,
        resume_label: "stub -r <session>",
        live_settable_params: &["auto_ready", "stall_secs", "silent_end_secs"],
        launch_params: &[
            "session",
            "upstream",
            "auto_ready",
            "silent_end_secs",
            "agents_md",
            "stall_secs",
        ],
        session_id_label: "Stub session",
        respond_rejection: None,
        capabilities: &[],
        doctor_caps: &[],
        probe_bins: &[],
        session_disposable: false,
        launch_default: false,
        internal: true,
    },
    EndpointSpec {
        provider: INBOX,
        endpoint_kind: INBOX,
        display: "Inbox (durable mailbox)",
        has_actor: false,
        attach: Attach::None,
        ready_gate: false,
        screen_probe: false,
        reports: Reporting::Explicit,
        report_hint: Reporting::Explicit,
        brokers_requests: false,
        resumable: false,
        resume_label: "n/a — mailbox",
        live_settable_params: &["inbox_warn_unread", "inbox_warn_idle_secs"],
        launch_params: &[],
        session_id_label: "none",
        respond_rejection: None,
        capabilities: &["inbox_endpoint"],
        doctor_caps: &["native_inbox_endpoint"],
        probe_bins: &[],
        session_disposable: false,
        launch_default: false,
        internal: false,
    },
    EndpointSpec {
        provider: "fake",
        endpoint_kind: "fake",
        display: "Fake (test double)",
        has_actor: true,
        attach: Attach::None,
        ready_gate: false,
        screen_probe: false,
        reports: Reporting::TurnResult,
        report_hint: Reporting::Explicit,
        brokers_requests: true,
        resumable: true,
        resume_label: "in-process double",
        live_settable_params: &["stall_secs"],
        launch_params: &["session", "upstream", "agents_md", "stall_secs"],
        session_id_label: "Fake session",
        respond_rejection: None,
        capabilities: &["fake_provider_tests"],
        doctor_caps: &["fake_provider_tests"],
        probe_bins: &[],
        session_disposable: false,
        launch_default: true,
        internal: false,
    },
];

/// `health.capabilities` entry for daemon-wide model defaults.
pub const MODEL_DEFAULTS_CAPABILITY: &str = "model_defaults";

/// Daemon-level features reported alongside the per-spec capability
/// names — not properties of any one endpoint.
const DAEMON_FEATURES: &[&str] = &[
    "agent_registry",
    "durable_queue",
    "operator_reconcile",
    "job_lifecycle",
    "revision_bound_verdicts",
    "approval_brokering",
    "result_routing",
    MODEL_DEFAULTS_CAPABILITY,
];

/// Whether this endpoint accepts a launch `model` param. Test doubles
/// and internal profiles are excluded even if a future spec lists one.
pub fn supports_model(provider: &str, kind: &str) -> bool {
    match spec_opt(provider, kind) {
        Some(spec) if !spec.internal && spec.provider != "fake" && provider != "fake" => {
            spec.launch_params.contains(&"model")
        }
        _ => false,
    }
}

/// One provider row for the settings matrix. Ineligible rows stay visible
/// so the operator can see why that provider has no model default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProviderRow {
    pub id: &'static str,
    pub eligible: bool,
    pub kinds: Vec<&'static str>,
    pub limitation: Option<&'static str>,
}

/// Public providers in registry order. Inbox, fake, and internal
/// profiles are omitted. Devin remains as an ineligible row.
pub fn model_provider_matrix() -> &'static [ModelProviderRow] {
    static ROWS: LazyLock<Vec<ModelProviderRow>> = LazyLock::new(|| {
        let mut rows: Vec<ModelProviderRow> = Vec::new();
        for spec in SPECS {
            if spec.internal || spec.provider == INBOX || spec.provider == "fake" {
                continue;
            }
            let eligible_kind = spec.launch_params.contains(&"model");
            if let Some(row) = rows.iter_mut().find(|row| row.id == spec.provider) {
                if !row.kinds.contains(&spec.endpoint_kind) {
                    row.kinds.push(spec.endpoint_kind);
                }
                if eligible_kind {
                    row.eligible = true;
                    row.limitation = None;
                }
                continue;
            }
            let limitation = if !eligible_kind && spec.provider == "devin" {
                Some(
                    "Devin does not accept a model at launch. Cadence cannot apply a model default to Devin agents.",
                )
            } else if !eligible_kind {
                Some("This provider does not accept a launch model.")
            } else {
                None
            };
            rows.push(ModelProviderRow {
                id: spec.provider,
                eligible: eligible_kind,
                kinds: vec![spec.endpoint_kind],
                limitation,
            });
        }
        rows
    });
    &ROWS
}

/// Providers whose settings may store a baseline.
pub fn model_provider_ids() -> Vec<&'static str> {
    model_provider_matrix()
        .iter()
        .filter(|row| row.eligible)
        .map(|row| row.id)
        .collect()
}

/// `health.capabilities`: daemon features plus each spec's contribution,
/// generated from the table — never hand-listed per provider.
pub fn capabilities() -> &'static [&'static str] {
    static CAPS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        // A capability several specs share (pty_verified_autoready)
        // lists once — the set is semantic, not per-pair.
        let mut caps: Vec<&'static str> = DAEMON_FEATURES.to_vec();
        for c in SPECS.iter().flat_map(|s| s.capabilities.iter().copied()) {
            if !caps.contains(&c) {
                caps.push(c);
            }
        }
        caps
    });
    &CAPS
}

/// Exact pair lookup. The `fake` endpoint kind is provider-agnostic —
/// it resolves to the test-double spec for any provider, mirroring the
/// factory's `fake` arm.
pub fn spec_opt(provider: &str, kind: &str) -> Option<&'static EndpointSpec> {
    SPECS
        .iter()
        .find(|s| s.provider == provider && s.endpoint_kind == kind)
        .or_else(|| {
            SPECS
                .iter()
                .find(|s| kind == "fake" && s.endpoint_kind == "fake")
        })
}

/// Validating lookup used by the factory: unknown pairs are rejected
/// with the supported combinations, reproducing the historical per-kind
/// error text ("No <kind> adapter for provider 'x' (implemented: …)",
/// "Endpoint kind 'x' is not implemented (implemented: …)").
pub fn spec(provider: &str, kind: &str) -> Result<&'static EndpointSpec> {
    if let Some(s) = spec_opt(provider, kind) {
        return Ok(s);
    }
    // Only actor-owning, user-facing kinds are buildable — a mailbox
    // pair is "not implemented" for factory purposes, and internal
    // test doubles are excluded from the supported-combination text,
    // matching the factory arms' literals verbatim.
    let providers: Vec<&str> = SPECS
        .iter()
        .filter(|s| s.has_actor && !s.internal && s.endpoint_kind == kind)
        .map(|s| s.provider)
        .collect();
    if providers.is_empty() {
        let mut kinds: Vec<&str> = Vec::new();
        for s in SPECS.iter().filter(|s| s.has_actor && !s.internal) {
            if !kinds.contains(&s.endpoint_kind) {
                kinds.push(s.endpoint_kind);
            }
        }
        Err(Error::rejected(format!(
            "Endpoint kind '{kind}' is not implemented \
             (implemented: {})",
            kinds.join(", ")
        )))
    } else {
        Err(Error::rejected(format!(
            "No {kind} adapter for provider '{provider}' \
             (implemented: {})",
            providers.join(", ")
        )))
    }
}

/// The provider's launch-verb endpoint kind (`cadence devin` → `pty`,
/// `cadence codex` → `managed-ws`). Providers without a launch verb are
/// rejected with the same text the inline match produced.
pub fn default_kind(provider: &str) -> Result<&'static str> {
    SPECS
        .iter()
        .find(|s| s.provider == provider && s.launch_default)
        .map(|s| s.endpoint_kind)
        .ok_or_else(|| {
            Error::rejected(format!(
                "Unknown provider '{provider}' — expected devin, codex, claude, cursor or fake"
            ))
        })
}

/// Provider is the mailbox provider.
pub fn is_inbox_provider(provider: &str) -> bool {
    provider == INBOX
}

/// Endpoint kind is the mailbox kind.
pub fn is_inbox_kind(kind: &str) -> bool {
    kind == INBOX
}

/// Does this endpoint own an actor/process. Pair lookup first, then
/// kind-level fallback (an unregistered pair like `devin`/`inbox` still
/// answers by kind, matching the old `!= "inbox"` checks exactly);
/// wholly unknown kinds default to `true` — they were never the
/// mailbox.
pub fn has_actor(provider: &str, kind: &str) -> bool {
    spec_opt(provider, kind)
        .or_else(|| SPECS.iter().find(|s| s.endpoint_kind == kind))
        .map(|s| s.has_actor)
        .unwrap_or(true)
}

/// The endpoint exposes an attachable surface (tmux pane or provider
/// TUI). Headless and actorless endpoints are not attachable.
pub fn attachable(provider: &str, kind: &str) -> bool {
    matches!(
        spec_opt(provider, kind).map(|s| s.attach),
        Some(Attach::Tmux) | Some(Attach::ProviderTui(_))
    )
}

/// Operator ready-claim gate exists on this endpoint (pty only).
pub fn ready_gate(provider: &str, kind: &str) -> bool {
    spec_opt(provider, kind)
        .map(|s| s.ready_gate)
        .unwrap_or(false)
}

/// Screen probe exists on this endpoint (pty only).
pub fn screen_probe(provider: &str, kind: &str) -> bool {
    spec_opt(provider, kind)
        .map(|s| s.screen_probe)
        .unwrap_or(false)
}

/// The adapter's turn result completes the message (managed/fake), so
/// the report contract is the final `SHA:` trailer — not `message result`.
pub fn reports_turn_result(provider: &str, kind: &str) -> bool {
    spec_opt(provider, kind)
        .map(|s| s.reports == Reporting::TurnResult)
        .unwrap_or(false)
}

/// Which report instruction the briefing prints for this endpoint.
pub fn report_hint(provider: &str, kind: &str) -> Reporting {
    spec_opt(provider, kind)
        .map(|s| s.report_hint)
        .unwrap_or(Reporting::Explicit)
}

/// Provider-specific `agent respond` rejection, if any.
pub fn respond_rejection(provider: &str, kind: &str) -> Option<&'static str> {
    spec_opt(provider, kind).and_then(|s| s.respond_rejection)
}

/// The concrete resume command for a stopped agent — the spec's
/// `resume_label` template with the agent's live ids filled in. `None`
/// where the endpoint has no meaningful resume surface (mailbox, test
/// double) or the agent has no native session recorded.
pub fn resume_command(
    provider: &str,
    kind: &str,
    thread_id: &str,
    session_id: &str,
    endpoint: &str,
) -> Option<String> {
    let s = spec_opt(provider, kind)?;
    if !s.resumable || thread_id.is_empty() {
        return None;
    }
    let label = s.resume_label;
    if label.contains("<slug>") {
        Some(label.replace("<slug>", thread_id))
    } else if label.contains("<session>") {
        let session = if session_id.is_empty() {
            thread_id
        } else {
            session_id
        };
        Some(label.replace("<session>", session))
    } else if !endpoint.is_empty() && label.starts_with("codex resume") {
        Some(format!("{label} {endpoint} {thread_id}"))
    } else {
        None
    }
}

/// `agent set` patch validation — the live-mutable allowlist is explicit:
/// arbitrary keys like `upstream` or `session` would silently rewire
/// routing and session binding, so they are rejected rather than merged.
/// Error text is the historical wording, verbatim.
pub fn validate_live_param(provider: &str, kind: &str, key: &str, value: &Value) -> Result<()> {
    match key {
        "auto_ready" => {
            if !screen_probe(provider, kind) {
                return Err(Error::rejected(
                    "'auto_ready' only applies to pty endpoints — \
                     a screen probe exists only there",
                ));
            }
            if !(value.is_null() || value.as_str() == Some("verified")) {
                return Err(Error::rejected(
                    "'auto_ready' accepts \"verified\" or a bare key \
                     (removal) — no other value is implemented",
                ));
            }
            Ok(())
        }
        "stall_secs" => {
            if !has_actor(provider, kind) {
                return Err(Error::rejected(
                    "'stall_secs' only applies to endpoints with an actor — \
                     there is no turn to watch without one",
                ));
            }
            if !check_stall_secs(value) {
                return Err(Error::rejected(
                    "'stall_secs' must be a non-negative integer (0 \
                     disables stall detection) or a bare key removal",
                ));
            }
            Ok(())
        }
        "silent_end_secs" => {
            if !screen_probe(provider, kind) {
                return Err(Error::rejected(
                    "'silent_end_secs' only applies to pty endpoints — \
                     idle-pane detection needs a screen probe",
                ));
            }
            if !check_stall_secs(value) {
                return Err(Error::rejected(
                    "'silent_end_secs' must be a non-negative integer (0 \
                     disables silent-end detection) or a bare key removal",
                ));
            }
            Ok(())
        }
        // CAD-251: unconsumed-inbox warning thresholds.
        "inbox_warn_unread" | "inbox_warn_idle_secs" => {
            if has_actor(provider, kind) {
                return Err(Error::rejected(format!(
                    "'{key}' only applies to inbox endpoints — a live \
                     endpoint consumes its own queue"
                )));
            }
            if !check_stall_secs(value) {
                return Err(Error::rejected(format!(
                    "'{key}' must be a non-negative integer or a bare key \
                     removal (back to the default)"
                )));
            }
            Ok(())
        }
        other => Err(Error::rejected(format!(
            "'{other}' is not live-settable — allowed keys: auto_ready \
             (pty only), stall_secs, silent_end_secs (pty only), \
             inbox_warn_unread, inbox_warn_idle_secs (inbox only). \
             Recreate the agent to change wiring params like upstream \
             or session"
        ))),
    }
}

/// The Claude CLI's `--effort` levels.
pub const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Reject an effort level the Claude CLI does not accept — the launch
/// verbs, `agent_register` and `agent set --next-launch` share this.
pub fn claude_effort(level: &str) -> Result<()> {
    if CLAUDE_EFFORTS.contains(&level) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "unknown claude effort '{level}' — expected one of: {}",
            CLAUDE_EFFORTS.join(", ")
        )))
    }
}

/// Launch params `agent set --next-launch` may change: stored for the
/// next open, never pushed to the live process.
const NEXT_LAUNCH_PARAMS: &[&str] = &["model", "effort", "approval_policy"];

/// Validate one `agent set --next-launch` key: `model`/`effort` only,
/// and only where the endpoint launches with that param. A null value
/// (bare key) clears it back to the provider default.
pub fn validate_next_launch_param(
    provider: &str,
    kind: &str,
    key: &str,
    value: &Value,
) -> Result<()> {
    let launches = spec(provider, kind)
        .map(|s| s.launch_params.contains(&key))
        .unwrap_or(false);
    if !NEXT_LAUNCH_PARAMS.contains(&key) || !launches {
        return Err(Error::rejected(format!(
            "'{key}' cannot be set for the next launch of a {provider}/{kind} \
             agent — --next-launch takes the launch params an endpoint \
             declares (claude: model, effort; cursor: model; codex: model, \
             effort, \
             approval_policy). Recreate the agent to change wiring \
             params like upstream or session"
        )));
    }
    match (key, value) {
        (_, Value::Null) => Ok(()),
        ("effort", Value::String(level)) if provider == "claude" => claude_effort(level),
        ("effort", Value::String(level)) if provider == "codex" => codex_effort(level),
        ("approval_policy", Value::String(policy)) => codex_approval_policy(policy),
        ("model", Value::String(m)) if !m.trim().is_empty() => Ok(()),
        _ => Err(Error::rejected(format!(
            "'{key}' needs a non-empty string value, or a bare key to clear it"
        ))),
    }
}

/// Reject a Devin permission mode outside the four-value vocabulary —
/// the launch verbs and `agent_register` share this check, and the
/// error always names every accepted value.
pub fn devin_permission_mode(mode: &str) -> Result<()> {
    if DEVIN_PERMISSION_MODES.contains(&mode) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "unknown devin permission mode '{mode}' — expected one of: {}",
            DEVIN_PERMISSION_MODES.join(", ")
        )))
    }
}

/// The Codex app-server's `approvalPolicy` vocabulary for
/// `thread/start`/`thread/resume` — `never` is the cadence worker
/// posture: no approval round-trips to stall an unattended turn on.
pub const CODEX_APPROVAL_POLICIES: &[&str] = &["never", "on-request", "on-failure", "untrusted"];

/// The Codex app-server effort vocabulary. The model metadata queried at
/// open time is authoritative for the pair: for example, Luna supports
/// `max` but does not advertise `ultra`, while Astra does.
pub const CODEX_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];

/// Reject an effort level outside the Codex app-server vocabulary. Whether
/// a particular model supports that level is checked against `model/list`
/// when the endpoint opens.
pub fn codex_effort(level: &str) -> Result<()> {
    if CODEX_EFFORTS.contains(&level) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "unknown codex effort '{level}' — expected one of: {}",
            CODEX_EFFORTS.join(", ")
        )))
    }
}

/// Reject a Codex approval policy outside the four-value vocabulary —
/// `agent_register`, `agent set --next-launch` and the adapter's open
/// share this check, and the error always names every accepted value.
pub fn codex_approval_policy(policy: &str) -> Result<()> {
    if CODEX_APPROVAL_POLICIES.contains(&policy) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "unknown codex approval_policy '{policy}' — expected one of: {}",
            CODEX_APPROVAL_POLICIES.join(", ")
        )))
    }
}

/// Reject a Cursor permission mode outside its two-value vocabulary —
/// each accepted value maps to one `cursor-agent` flag.
pub fn cursor_permission_mode(mode: &str) -> Result<()> {
    if CURSOR_PERMISSION_MODES.contains(&mode) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "unknown cursor permission mode '{mode}' — expected one of: {}",
            CURSOR_PERMISSION_MODES.join(", ")
        )))
    }
}

/// `stall_secs` accepts an unsigned integer, a digit string (`agent
/// set` values arrive as strings), or null — shared by the launch and
/// live-set checks so both reject the same values.
fn check_stall_secs(value: &Value) -> bool {
    value.is_null()
        || value.as_u64().is_some()
        || value.as_str().is_some_and(|s| s.parse::<u64>().is_ok())
}

/// Register-time validation for enumerated launch params. Params not
/// named here keep their historical pass-through (claude's modes are
/// provider-validated — its own CLI rejects bad values on spawn).
pub fn validate_launch_params(provider: &str, kind: &str, params: &Value) -> Result<()> {
    if let Some(v) = params.get("stall_secs") {
        if !has_actor(provider, kind) {
            return Err(Error::rejected(
                "'stall_secs' only applies to endpoints with an actor — \
                 there is no turn to watch without one",
            ));
        }
        if !check_stall_secs(v) {
            return Err(Error::rejected(
                "'stall_secs' must be a non-negative integer (0 disables \
                 stall detection) or a bare key removal",
            ));
        }
    }
    if let Some(v) = params.get("silent_end_secs") {
        if !screen_probe(provider, kind) {
            return Err(Error::rejected(
                "'silent_end_secs' only applies to pty endpoints — \
                 idle-pane detection needs a screen probe",
            ));
        }
        if !check_stall_secs(v) {
            return Err(Error::rejected(
                "'silent_end_secs' must be a non-negative integer (0 \
                 disables silent-end detection) or a bare key removal",
            ));
        }
    }
    if provider == "devin" && kind == "pty" {
        if let Some(v) = params.get("permission_mode") {
            match v.as_str() {
                Some(mode) => devin_permission_mode(mode)?,
                None => {
                    return Err(Error::rejected(format!(
                        "devin permission_mode must be a string, one of: {}",
                        DEVIN_PERMISSION_MODES.join(", ")
                    )))
                }
            }
        }
    }
    if provider == "codex" {
        if let Some(v) = params.get("model") {
            match v.as_str() {
                Some(model) if !model.trim().is_empty() => {}
                _ => return Err(Error::rejected("codex model must be a non-empty string")),
            }
        }
        if let Some(v) = params.get("effort") {
            match v.as_str() {
                Some(level) => codex_effort(level)?,
                None => {
                    return Err(Error::rejected(format!(
                        "codex effort must be a string, one of: {}",
                        CODEX_EFFORTS.join(", ")
                    )))
                }
            }
        }
        if let Some(v) = params.get("approval_policy") {
            match v.as_str() {
                Some(policy) => codex_approval_policy(policy)?,
                None => {
                    return Err(Error::rejected(format!(
                        "codex approval_policy must be a string, one of: {}",
                        CODEX_APPROVAL_POLICIES.join(", ")
                    )))
                }
            }
        }
    }
    if provider == "claude" {
        if let Some(v) = params.get("effort") {
            match v.as_str() {
                Some(level) => claude_effort(level)?,
                None => {
                    return Err(Error::rejected(format!(
                        "claude effort must be a string, one of: {}",
                        CLAUDE_EFFORTS.join(", ")
                    )))
                }
            }
        }
    }
    if provider == "cursor" && kind == "pty" {
        if let Some(v) = params.get("permission_mode") {
            match v.as_str() {
                Some(mode) => cursor_permission_mode(mode)?,
                None => {
                    return Err(Error::rejected(format!(
                        "cursor permission_mode must be a string, one of: {}",
                        CURSOR_PERMISSION_MODES.join(", ")
                    )))
                }
            }
        }
    }
    if provider == "claude" && kind == "managed" {
        if let Some(v) = params.get("broker_approvals") {
            if !v.is_boolean() {
                return Err(Error::rejected(
                    "claude broker_approvals must be a boolean (set by --broker-approvals)",
                ));
            }
        }
        if let Some(v) = params.get("permission_timeout_secs") {
            if v.as_u64().is_none_or(|s| s < 1) {
                return Err(Error::rejected(
                    "claude permission_timeout_secs must be a positive integer \
                     (set by --permission-timeout-secs)",
                ));
            }
        }
        let brokered = params
            .get("broker_approvals")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let bypassed =
            params.get("permission_mode").and_then(Value::as_str) == Some("bypassPermissions");
        if brokered && bypassed {
            return Err(Error::rejected(
                "broker_approvals and bypassPermissions are incompatible — \
                 bypass makes every prompt moot",
            ));
        }
    }
    Ok(())
}

impl Attach {
    fn label(&self) -> &'static str {
        match self {
            Attach::None => "none",
            Attach::Headless => "headless",
            Attach::Tmux => "tmux",
            Attach::ProviderTui(_) => "provider_tui",
        }
    }
}

impl EndpointSpec {
    /// The `capabilities` object `agent show`/`agent list` emit.
    pub fn to_json(&self) -> Value {
        json!({
            "provider": self.provider,
            "endpoint_kind": self.endpoint_kind,
            "display": self.display,
            "has_actor": self.has_actor,
            "attach": self.attach.label(),
            "ready_gate": self.ready_gate,
            "screen_probe": self.screen_probe,
            "reports": match self.reports {
                Reporting::Explicit => "explicit",
                Reporting::TurnResult => "turn_result",
            },
            "brokers_requests": self.brokers_requests,
            "resumable": self.resumable,
            "resume": self.resume_label,
            "live_settable_params": self.live_settable_params,
            "launch_params": self.launch_params,
            "session_id_label": self.session_id_label,
            "internal": self.internal,
        })
    }
}

/// `agent show`/`agent list` helper: the spec's capabilities object, or
/// `null` when the registered pair has no spec.
pub fn capabilities_json(provider: &str, kind: &str) -> Value {
    spec_opt(provider, kind)
        .map(|s| s.to_json())
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_factory_pair_has_a_spec_and_back() {
        // Every (provider, kind) the factory constructs has a spec…
        for (provider, kind) in [
            ("codex", "managed"),
            ("codex", "managed-ws"),
            ("claude", "managed"),
            ("claude", "pty"),
            ("devin", "pty"),
            ("cursor", "pty"),
            ("tui-stub", "pty"),
            ("fake", "fake"),
        ] {
            assert!(spec_opt(provider, kind).is_some(), "{provider}/{kind}");
            assert!(spec(provider, kind).is_ok(), "{provider}/{kind}");
        }
        // …and every actor-owning spec constructs through the factory —
        // the pair table and the match arms cannot drift apart.
        for s in SPECS.iter().filter(|s| s.has_actor) {
            assert!(spec(s.provider, s.endpoint_kind).is_ok());
        }
    }

    #[test]
    fn fake_kind_is_provider_agnostic() {
        let s = spec_opt("devin", "fake").unwrap();
        assert_eq!(s.provider, "fake");
        assert!(s.has_actor);
    }

    #[test]
    fn unknown_pairs_reject_with_supported_combos() {
        assert_eq!(
            spec("codex", "pty").unwrap_err().to_string(),
            "No pty adapter for provider 'codex' (implemented: claude, devin, cursor)"
        );
        assert_eq!(
            spec("devin", "managed").unwrap_err().to_string(),
            "No managed adapter for provider 'devin' (implemented: codex, claude)"
        );
        assert_eq!(
            spec("devin", "bogus").unwrap_err().to_string(),
            "Endpoint kind 'bogus' is not implemented \
             (implemented: managed, managed-ws, pty, fake)"
        );
    }

    #[test]
    fn generated_capabilities_cover_the_legacy_list() {
        let caps = capabilities();
        for name in [
            "agent_registry",
            "durable_queue",
            "managed_codex_stdio",
            "managed_codex_ws",
            "managed_claude_stream",
            "pty_claude_tmux",
            "pty_devin_tmux",
            "pty_cursor_tmux",
            "pty_verified_autoready",
            "operator_reconcile",
            "inbox_endpoint",
            "job_lifecycle",
            "revision_bound_verdicts",
            "approval_brokering",
            "result_routing",
            "model_defaults",
            "fake_provider_tests",
        ] {
            assert!(caps.contains(&name), "missing {name}");
        }
        assert_eq!(caps.len(), 17);
    }

    #[test]
    fn model_matrix_keeps_devin_and_drops_test_doubles() {
        let rows = model_provider_matrix();
        let ids: Vec<&str> = rows.iter().map(|row| row.id).collect();
        assert_eq!(ids, vec!["codex", "claude", "devin", "cursor"]);
        assert!(rows.iter().any(|row| row.id == "claude" && row.eligible));
        assert!(rows
            .iter()
            .any(|row| { row.id == "devin" && !row.eligible && row.limitation.is_some() }));
        assert!(rows.iter().all(|row| row.id != "fake" && row.id != "inbox"));
        assert!(supports_model("claude", "managed"));
        assert!(supports_model("cursor", "pty"));
        assert!(!supports_model("devin", "pty"));
        assert!(!supports_model("fake", "fake"));
        assert!(!supports_model("inbox", "inbox"));
    }

    #[test]
    fn resume_commands_fill_agent_ids() {
        assert_eq!(
            resume_command("devin", "pty", "devin-abc123", "", "tmux://s/w"),
            Some("devin -r devin-abc123".to_string())
        );
        assert_eq!(
            resume_command("claude", "managed", "t1", "claude-sess-9", ""),
            Some("claude --resume claude-sess-9".to_string())
        );
        assert_eq!(
            resume_command("codex", "managed-ws", "thr-7", "", "ws://127.0.0.1:9/x"),
            Some("codex resume --remote ws://127.0.0.1:9/x thr-7".to_string())
        );
        // No endpoint → no remote resume command; mailbox/fake → none.
        assert_eq!(resume_command("codex", "managed", "thr-7", "", ""), None);
        assert_eq!(resume_command("inbox", "inbox", "x", "", ""), None);
        assert_eq!(resume_command("fake", "fake", "", "", ""), None);
        // A dead agent with no thread has nothing to resume.
        assert_eq!(resume_command("devin", "pty", "", "", ""), None);
    }

    #[test]
    fn kind_level_fallbacks_match_legacy_checks() {
        assert!(!has_actor("inbox", "inbox"));
        // An unregistered mailbox pair still answers by kind.
        assert!(!has_actor("devin", "inbox"));
        assert!(has_actor("devin", "pty"));
        // Wholly unknown kinds default to actor-owning (never mailbox).
        assert!(has_actor("devin", "bogus"));
        assert!(attachable("devin", "pty"));
        assert!(attachable("codex", "managed-ws"));
        assert!(!attachable("claude", "managed"));
        assert!(!attachable("inbox", "inbox"));
        assert!(ready_gate("devin", "pty"));
        assert!(!ready_gate("codex", "managed-ws"));
        assert!(screen_probe("devin", "pty"));
        assert!(reports_turn_result("claude", "managed"));
        assert!(reports_turn_result("fake", "fake"));
        assert!(!reports_turn_result("devin", "pty"));
        assert_eq!(report_hint("claude", "managed"), Reporting::TurnResult);
        assert_eq!(report_hint("codex", "managed-ws"), Reporting::Explicit);
        assert!(respond_rejection("claude", "managed").is_some());
        assert!(respond_rejection("codex", "managed-ws").is_none());
    }

    #[test]
    fn devin_permission_modes_validated() {
        for mode in DEVIN_PERMISSION_MODES {
            assert!(devin_permission_mode(mode).is_ok(), "{mode}");
        }
        // Everything else rejects — including claude's own vocabulary —
        // and the error always lists the four accepted values.
        for bad in [
            "bypass",
            "manual",
            "acceptEdits",
            "bypassPermissions",
            "",
            "AUTO",
        ] {
            let msg = devin_permission_mode(bad).unwrap_err().to_string();
            for accepted in DEVIN_PERMISSION_MODES {
                assert!(
                    msg.contains(accepted),
                    "'{bad}' error missing '{accepted}': {msg}"
                );
            }
        }
    }

    #[test]
    fn devin_launch_params_validated_at_register() {
        // Only (devin, pty) carries the enumerated check — other pairs
        // pass params through untouched.
        for mode in DEVIN_PERMISSION_MODES {
            assert!(
                validate_launch_params("devin", "pty", &json!({"permission_mode": mode})).is_ok(),
                "{mode}"
            );
        }
        let msg = validate_launch_params("devin", "pty", &json!({"permission_mode": "bogus"}))
            .unwrap_err()
            .to_string();
        for accepted in DEVIN_PERMISSION_MODES {
            assert!(msg.contains(accepted), "missing '{accepted}': {msg}");
        }
        // Non-string values reject too; unrelated keys pass through.
        assert!(validate_launch_params("devin", "pty", &json!({"permission_mode": 1})).is_err());
        assert!(validate_launch_params(
            "devin",
            "pty",
            &json!({"session": "s", "upstream": "pm", "auto_ready": "verified"})
        )
        .is_ok());
        assert!(
            validate_launch_params("claude", "managed", &json!({"permission_mode": "bogus"}))
                .is_ok()
        );
    }

    #[test]
    fn cursor_permission_modes_validated() {
        for mode in CURSOR_PERMISSION_MODES {
            assert!(cursor_permission_mode(mode).is_ok(), "{mode}");
        }
        // Everything else rejects — including devin's own vocabulary —
        // and the error always lists the two accepted values.
        for bad in ["auto", "smart", "dangerous", "bypass", "", "FORCE"] {
            let msg = cursor_permission_mode(bad).unwrap_err().to_string();
            for accepted in CURSOR_PERMISSION_MODES {
                assert!(
                    msg.contains(accepted),
                    "'{bad}' error missing '{accepted}': {msg}"
                );
            }
        }
    }

    #[test]
    fn cursor_launch_params_validated_at_register() {
        for mode in CURSOR_PERMISSION_MODES {
            assert!(
                validate_launch_params("cursor", "pty", &json!({"permission_mode": mode})).is_ok(),
                "{mode}"
            );
        }
        let msg = validate_launch_params("cursor", "pty", &json!({"permission_mode": "bogus"}))
            .unwrap_err()
            .to_string();
        for accepted in CURSOR_PERMISSION_MODES {
            assert!(msg.contains(accepted), "missing '{accepted}': {msg}");
        }
        // Non-string values reject too; unrelated keys pass through.
        assert!(validate_launch_params("cursor", "pty", &json!({"permission_mode": 1})).is_err());
        assert!(validate_launch_params(
            "cursor",
            "pty",
            &json!({"session": "c", "model": "g", "upstream": "pm", "auto_ready": "verified"})
        )
        .is_ok());
    }

    #[test]
    fn codex_approval_policies_validated() {
        for policy in CODEX_APPROVAL_POLICIES {
            assert!(codex_approval_policy(policy).is_ok(), "{policy}");
        }
        // Everything else rejects — the error always lists the four
        // accepted values so the caller can self-correct.
        for bad in ["auto", "always", "bypass", "", "NEVER", "on_request"] {
            let msg = codex_approval_policy(bad).unwrap_err().to_string();
            for accepted in CODEX_APPROVAL_POLICIES {
                assert!(
                    msg.contains(accepted),
                    "'{bad}' error missing '{accepted}': {msg}"
                );
            }
        }
    }

    #[test]
    fn codex_launch_params_validated_at_register() {
        // Both codex endpoints declare and enforce the same vocabulary.
        for kind in ["managed", "managed-ws"] {
            for policy in CODEX_APPROVAL_POLICIES {
                assert!(
                    validate_launch_params("codex", kind, &json!({"approval_policy": policy}))
                        .is_ok(),
                    "{kind} {policy}"
                );
            }
            let msg = validate_launch_params("codex", kind, &json!({"approval_policy": "bogus"}))
                .unwrap_err()
                .to_string();
            for accepted in CODEX_APPROVAL_POLICIES {
                assert!(msg.contains(accepted), "missing '{accepted}': {msg}");
            }
            // Non-string values reject too; unrelated keys pass through.
            assert!(validate_launch_params("codex", kind, &json!({"approval_policy": 1})).is_err());
            assert!(validate_launch_params(
                "codex",
                kind,
                &json!({"session": "s", "upstream": "pm", "stall_secs": 60})
            )
            .is_ok());
        }
        // Other providers pass the key through untouched.
        assert!(
            validate_launch_params("claude", "managed", &json!({"approval_policy": "bogus"}))
                .is_ok()
        );
    }

    #[test]
    fn codex_approval_policy_is_next_launch_settable() {
        for kind in ["managed", "managed-ws"] {
            // A valid value stores for the next open; null clears it.
            assert!(
                validate_next_launch_param("codex", kind, "approval_policy", &json!("on-failure"))
                    .is_ok(),
                "{kind}"
            );
            assert!(
                validate_next_launch_param("codex", kind, "approval_policy", &Value::Null).is_ok(),
                "{kind}"
            );
            // A bogus value names all four accepted values.
            let msg = validate_next_launch_param("codex", kind, "approval_policy", &json!("bogus"))
                .unwrap_err()
                .to_string();
            for accepted in CODEX_APPROVAL_POLICIES {
                assert!(msg.contains(accepted), "missing '{accepted}': {msg}");
            }
        }
        // Endpoints that do not declare it refuse the key entirely.
        let msg =
            validate_next_launch_param("claude", "managed", "approval_policy", &json!("never"))
                .unwrap_err()
                .to_string();
        assert!(msg.contains("cannot be set"), "{msg}");
    }

    #[test]
    fn permission_mode_is_not_live_settable() {
        // Launch-only: `agent set` must refuse it — a live patch would
        // silently diverge the stored mode from the running pane.
        for pair in [("devin", "pty"), ("claude", "managed"), ("cursor", "pty")] {
            let msg = validate_live_param(pair.0, pair.1, "permission_mode", &json!("smart"))
                .unwrap_err()
                .to_string();
            assert!(msg.contains("not live-settable"), "{msg}");
        }
    }
}
