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
        live_settable_params: &[],
        launch_params: &["session", "upstream"],
        session_id_label: "Codex thread",
        respond_rejection: None,
        capabilities: &["managed_codex_stdio"],
        doctor_caps: &["managed_codex_stdio"],
        probe_bins: &[("codex", &["--version"])],
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
        live_settable_params: &[],
        launch_params: &["session", "upstream"],
        session_id_label: "Codex thread",
        respond_rejection: None,
        capabilities: &["managed_codex_ws"],
        doctor_caps: &["managed_codex_ws"],
        probe_bins: &[("codex", &["--version"])],
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
        brokers_requests: false,
        resumable: true,
        resume_label: "claude --resume <session>",
        live_settable_params: &[],
        launch_params: &[
            "model",
            "permission_mode",
            "allowed_tools",
            "turn_idle_secs",
            "turn_max_secs",
            "session",
            "upstream",
        ],
        session_id_label: "Claude session",
        respond_rejection: Some(
            "managed claude endpoints broker no requests — widen \
             permissions by relaunching or rejoining with \
             `--permission-mode <mode>` or `--allow \"<pattern>\"` \
             (or `--bypass`); denials are recorded as \
             permission_denied events on the agent",
        ),
        capabilities: &["managed_claude_stream"],
        doctor_caps: &["managed_claude_stream"],
        probe_bins: &[("claude", &["--version"])],
        launch_default: true,
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
        live_settable_params: &["auto_ready"],
        launch_params: &["session", "upstream", "auto_ready"],
        session_id_label: "Devin session",
        respond_rejection: None,
        capabilities: &["pty_devin_tmux", "pty_verified_autoready"],
        doctor_caps: &["pty_devin_tmux"],
        probe_bins: &[("devin", &["--version"]), ("tmux", &["-V"])],
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
        live_settable_params: &["auto_ready"],
        launch_params: &["session", "upstream", "auto_ready"],
        session_id_label: "Stub session",
        respond_rejection: None,
        capabilities: &[],
        doctor_caps: &[],
        probe_bins: &[],
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
        live_settable_params: &[],
        launch_params: &[],
        session_id_label: "none",
        respond_rejection: None,
        capabilities: &["inbox_endpoint"],
        doctor_caps: &["native_inbox_endpoint"],
        probe_bins: &[],
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
        live_settable_params: &[],
        launch_params: &["session", "upstream"],
        session_id_label: "Fake session",
        respond_rejection: None,
        capabilities: &["fake_provider_tests"],
        doctor_caps: &["fake_provider_tests"],
        probe_bins: &[],
        launch_default: true,
        internal: false,
    },
];

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
];

/// `health.capabilities`: daemon features plus each spec's contribution,
/// generated from the table — never hand-listed per provider.
pub fn capabilities() -> &'static [&'static str] {
    static CAPS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        DAEMON_FEATURES
            .iter()
            .copied()
            .chain(SPECS.iter().flat_map(|s| s.capabilities.iter().copied()))
            .collect()
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
                "Unknown provider '{provider}' — expected devin, codex, claude or fake"
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
        other => Err(Error::rejected(format!(
            "'{other}' is not live-settable — allowed keys: auto_ready \
             (pty only). Recreate the agent to change wiring params \
             like upstream or session"
        ))),
    }
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
            ("devin", "pty"),
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
            "No pty adapter for provider 'codex' (implemented: devin)"
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
            "pty_devin_tmux",
            "pty_verified_autoready",
            "operator_reconcile",
            "inbox_endpoint",
            "job_lifecycle",
            "revision_bound_verdicts",
            "approval_brokering",
            "result_routing",
            "fake_provider_tests",
        ] {
            assert!(caps.contains(&name), "missing {name}");
        }
        assert_eq!(caps.len(), 14);
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
}
