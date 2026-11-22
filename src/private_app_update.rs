//! Pre-dispatch readiness and updater agent resolution for managed private app updates.
//!
//! Keeps execution authority separated from conversational Personal Assistant
//! (`service/*`) identities and CAD-2434's chat-only "Default app agent".

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Why a candidate updater cannot execute a private app update right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateAppReadinessReason {
    MissingIdentity,
    PersonalAssistantExcluded,
    ChatDefaultNotUpdater,
    MissingWriteScope,
    AdapterUploadUnsupported,
    UnpairedWorkspaceMcp,
    BoardUnavailable,
    ActorMismatch,
    ServerNotReady,
}

impl PrivateAppReadinessReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingIdentity => "missing_identity",
            Self::PersonalAssistantExcluded => "personal_assistant_excluded",
            Self::ChatDefaultNotUpdater => "chat_default_not_updater",
            Self::MissingWriteScope => "missing_write_scope",
            Self::AdapterUploadUnsupported => "adapter_upload_unsupported",
            Self::UnpairedWorkspaceMcp => "unpaired_workspace_mcp",
            Self::BoardUnavailable => "board_unavailable",
            Self::ActorMismatch => "actor_mismatch",
            Self::ServerNotReady => "server_not_ready",
        }
    }
}

/// Format readiness refusal reasons into a deterministic, non-sensitive error string.
pub fn format_readiness_refusal(reasons: &[PrivateAppReadinessReason]) -> String {
    let codes = if reasons.is_empty() {
        PrivateAppReadinessReason::MissingIdentity
            .as_str()
            .to_string()
    } else {
        reasons
            .iter()
            .map(|r| r.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("private app updater is not ready ({codes}); update aborted before staging package")
}

/// Which transport adapter executes the update.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateAppAdapterKind {
    CodexMcpAgent,
    WorkspaceMcp,
    Unsupported,
}

impl PrivateAppAdapterKind {
    /// Map an endpoint adapter string (`"{provider}/{endpoint_kind}"`) to upload support.
    pub fn from_endpoint_adapter(adapter: Option<&str>) -> Self {
        match adapter {
            Some("codex/managed" | "codex/managed-ws") => Self::CodexMcpAgent,
            Some(value) if value == "workspace_mcp" || value.starts_with("workspace_mcp/") => {
                Self::WorkspaceMcp
            }
            _ => Self::Unsupported,
        }
    }
}

/// Server-reported readiness snapshot from Workspace MCP `connection_status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerPrivateAppReadiness {
    pub ready: bool,
    pub actor: Option<String>,
    #[serde(default)]
    pub reasons: Vec<String>,
}

/// A candidate agent profile + connection state to evaluate before dispatching an update.
#[derive(Clone, Debug)]
pub struct PrivateAppUpdaterCandidate<'a> {
    /// Acting principal, expected as `agents/<profile-id>`.
    pub actor: Option<&'a str>,
    /// Explicit guard so CAD-2434's chat-only selector is never promoted silently.
    pub is_chat_default_only: bool,
    /// Credential/grant capabilities when known (`cli.write` required).
    pub capabilities: Option<&'a [String]>,
    /// Profile opt-in tool groups (`CAD-2561`: `cadence_writes` when enrolled).
    pub enabled_tools: Option<&'a [String]>,
    pub adapter: PrivateAppAdapterKind,
    /// Only inspected for `WorkspaceMcp`.
    pub workspace_mcp_paired: Option<bool>,
    /// `Some(false)` when board is inactive/unplaced; `None` when unknown.
    pub board_active: Option<bool>,
    /// Additive `AOS-690` `connection_status.private_app_readiness`; `None` on older hosts.
    pub server_readiness: Option<&'a ServerPrivateAppReadiness>,
}

/// Where the chosen updater agent came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateAppUpdaterSource {
    ExplicitAgent,
    DedicatedAppUpdater,
    WorkspaceDefaultUpdater,
}

/// Resolution inputs in priority order: explicit -> dedicated -> workspace default.
#[derive(Clone, Debug, Default)]
pub struct PrivateAppUpdaterResolution<'a> {
    pub explicit_agent: Option<PrivateAppUpdaterCandidate<'a>>,
    pub dedicated_app_updater: Option<PrivateAppUpdaterCandidate<'a>>,
    pub workspace_default_updater: Option<PrivateAppUpdaterCandidate<'a>>,
    /// Set when only a chat-only CAD-2434 app agent exists and no updater is configured.
    pub has_chat_default_app_agent: bool,
    /// Optional CAD-2434 chat-only default actor (`agents/<id>`), never admissible as an updater.
    pub chat_default_actor: Option<&'a str>,
    /// Set when the caller tried to route through Personal Assistant (`service/*`).
    pub personal_assistant_requested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPrivateAppUpdater {
    pub source: PrivateAppUpdaterSource,
    pub actor: String,
}

pub fn valid_package_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn valid_sha256_prefixed(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub fn valid_install_id(value: &str) -> bool {
    value.strip_prefix("install_").is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 120
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

pub fn valid_generation(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.chars().all(|c| !c.is_control())
}

pub fn valid_zip_file_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.ends_with(".zip")
        && !value.contains(['/', '\\', '\0'])
        && !value.contains("..")
        && value.chars().all(|c| !c.is_control())
}

fn valid_agent_actor(actor: &str) -> bool {
    let Some(id) = actor.strip_prefix("agents/") else {
        return false;
    };
    !id.is_empty()
        && id.len() <= 120
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn push_reason(reasons: &mut Vec<PrivateAppReadinessReason>, reason: PrivateAppReadinessReason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn validate_actor_value(value: &str, reasons: &mut Vec<PrivateAppReadinessReason>) {
    if value.starts_with("service/") {
        push_reason(
            reasons,
            PrivateAppReadinessReason::PersonalAssistantExcluded,
        );
    } else if !valid_agent_actor(value) {
        push_reason(reasons, PrivateAppReadinessReason::MissingIdentity);
    }
}

/// Evaluate whether a candidate can stage ZIP artifacts and execute private app updates.
pub fn check_private_app_readiness(
    candidate: &PrivateAppUpdaterCandidate<'_>,
) -> std::result::Result<(), Vec<PrivateAppReadinessReason>> {
    let mut reasons = Vec::new();
    if candidate.is_chat_default_only {
        push_reason(
            &mut reasons,
            PrivateAppReadinessReason::ChatDefaultNotUpdater,
        );
    }

    let server_actor = candidate.server_readiness.and_then(|s| s.actor.as_deref());
    match (candidate.actor, server_actor) {
        (None, None) => {
            push_reason(&mut reasons, PrivateAppReadinessReason::MissingIdentity);
        }
        (local, server) => {
            if let Some(value) = local {
                validate_actor_value(value, &mut reasons);
            }
            if let Some(value) = server {
                validate_actor_value(value, &mut reasons);
            }
            if let (Some(local_actor), Some(server_actor_val)) = (local, server) {
                if local_actor != server_actor_val {
                    push_reason(&mut reasons, PrivateAppReadinessReason::ActorMismatch);
                }
            }
        }
    }

    let has_cli_write = candidate
        .capabilities
        .is_some_and(|caps| caps.iter().any(|c| c == "cli.write"));
    let has_opt_in_write = candidate.enabled_tools.is_some_and(|tools| {
        tools
            .iter()
            .any(|t| matches!(t.as_str(), "cadence_writes" | "cadence.write" | "cli.write"))
    });
    let missing_write = match (candidate.capabilities, candidate.enabled_tools) {
        (Some(_), Some(_)) => !has_cli_write || !has_opt_in_write,
        (Some(_), None) => !has_cli_write,
        (None, Some(_)) => !has_opt_in_write,
        (None, None) => true,
    };
    if missing_write {
        push_reason(&mut reasons, PrivateAppReadinessReason::MissingWriteScope);
    }

    match candidate.adapter {
        PrivateAppAdapterKind::CodexMcpAgent => {}
        PrivateAppAdapterKind::WorkspaceMcp => {
            if candidate.workspace_mcp_paired != Some(true) {
                push_reason(
                    &mut reasons,
                    PrivateAppReadinessReason::UnpairedWorkspaceMcp,
                );
            }
        }
        PrivateAppAdapterKind::Unsupported => {
            push_reason(
                &mut reasons,
                PrivateAppReadinessReason::AdapterUploadUnsupported,
            );
        }
    }

    if candidate.board_active == Some(false) {
        push_reason(&mut reasons, PrivateAppReadinessReason::BoardUnavailable);
    }

    if let Some(server) = candidate.server_readiness {
        for raw in &server.reasons {
            let mapped = match raw.as_str() {
                "board_unavailable" => PrivateAppReadinessReason::BoardUnavailable,
                "missing_cli_scope" | "missing_write_scope" => {
                    PrivateAppReadinessReason::MissingWriteScope
                }
                "unpaired_workspace_mcp" => PrivateAppReadinessReason::UnpairedWorkspaceMcp,
                "personal_assistant_excluded" | "agent_required" => {
                    PrivateAppReadinessReason::PersonalAssistantExcluded
                }
                "missing_identity" => PrivateAppReadinessReason::MissingIdentity,
                _ => PrivateAppReadinessReason::ServerNotReady,
            };
            push_reason(&mut reasons, mapped);
        }
        if !server.ready && server.reasons.is_empty() {
            push_reason(&mut reasons, PrivateAppReadinessReason::ServerNotReady);
        }
    }

    if reasons.is_empty() {
        Ok(())
    } else {
        Err(reasons)
    }
}

/// Resolve the executing agent in strict priority order:
/// explicit agent -> app's dedicated updater -> workspace default updater.
/// Never falls back to Personal Assistant (`service/*`) or CAD-2434 chat defaults.
pub fn resolve_private_app_updater(
    input: &PrivateAppUpdaterResolution<'_>,
) -> std::result::Result<ResolvedPrivateAppUpdater, Vec<PrivateAppReadinessReason>> {
    let chosen = input
        .explicit_agent
        .as_ref()
        .map(|c| (PrivateAppUpdaterSource::ExplicitAgent, c))
        .or_else(|| {
            input
                .dedicated_app_updater
                .as_ref()
                .map(|c| (PrivateAppUpdaterSource::DedicatedAppUpdater, c))
        })
        .or_else(|| {
            input
                .workspace_default_updater
                .as_ref()
                .map(|c| (PrivateAppUpdaterSource::WorkspaceDefaultUpdater, c))
        });

    let Some((source, candidate)) = chosen else {
        return Err(if input.personal_assistant_requested {
            vec![PrivateAppReadinessReason::PersonalAssistantExcluded]
        } else if input.has_chat_default_app_agent {
            vec![PrivateAppReadinessReason::ChatDefaultNotUpdater]
        } else {
            vec![PrivateAppReadinessReason::MissingIdentity]
        });
    };

    if input.chat_default_actor.is_some_and(|chat| {
        candidate.actor == Some(chat)
            || candidate.server_readiness.and_then(|s| s.actor.as_deref()) == Some(chat)
    }) {
        return Err(vec![PrivateAppReadinessReason::ChatDefaultNotUpdater]);
    }
    check_private_app_readiness(candidate)?;
    let actor = candidate
        .actor
        .or(candidate.server_readiness.and_then(|s| s.actor.as_deref()))
        .ok_or_else(|| vec![PrivateAppReadinessReason::MissingIdentity])?
        .to_string();
    Ok(ResolvedPrivateAppUpdater { source, actor })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_candidate<'a>(
        actor: Option<&'a str>,
        caps: &'a [String],
    ) -> PrivateAppUpdaterCandidate<'a> {
        PrivateAppUpdaterCandidate {
            actor,
            is_chat_default_only: false,
            capabilities: Some(caps),
            enabled_tools: None,
            adapter: PrivateAppAdapterKind::CodexMcpAgent,
            workspace_mcp_paired: None,
            board_active: Some(true),
            server_readiness: None,
        }
    }

    #[test]
    fn readiness_excludes_personal_assistant_chat_default_missing_scope_and_unsupported() {
        let write_caps = vec!["cli.read".into(), "cli.write".into()];
        let read_caps = vec!["cli.read".into()];

        assert_eq!(
            check_private_app_readiness(&ready_candidate(Some("service/pa_1"), &write_caps)),
            Err(vec![PrivateAppReadinessReason::PersonalAssistantExcluded])
        );

        let mut chat = ready_candidate(Some("agents/ap_chat"), &write_caps);
        chat.is_chat_default_only = true;
        assert_eq!(
            check_private_app_readiness(&chat),
            Err(vec![PrivateAppReadinessReason::ChatDefaultNotUpdater])
        );

        assert_eq!(
            check_private_app_readiness(&ready_candidate(Some("agents/ap_1"), &read_caps)),
            Err(vec![PrivateAppReadinessReason::MissingWriteScope])
        );

        let mut unsupported = ready_candidate(Some("agents/ap_1"), &write_caps);
        unsupported.adapter = PrivateAppAdapterKind::Unsupported;
        assert_eq!(
            check_private_app_readiness(&unsupported),
            Err(vec![PrivateAppReadinessReason::AdapterUploadUnsupported])
        );

        let mut unpaired = ready_candidate(Some("agents/ap_1"), &write_caps);
        unpaired.adapter = PrivateAppAdapterKind::WorkspaceMcp;
        unpaired.workspace_mcp_paired = Some(false);
        assert_eq!(
            check_private_app_readiness(&unpaired),
            Err(vec![PrivateAppReadinessReason::UnpairedWorkspaceMcp])
        );
    }

    #[test]
    fn readiness_accepts_codex_and_degrades_cleanly_when_server_readiness_absent() {
        let write_caps = vec!["cli.write".into()];
        let candidate = ready_candidate(Some("agents/ap_updater"), &write_caps);
        assert_eq!(check_private_app_readiness(&candidate), Ok(()));

        // CAD-2561 opt-in profile flag without explicit token capabilities still validates.
        let enabled = vec!["cadence_writes".into()];
        let opt_in = PrivateAppUpdaterCandidate {
            actor: Some("agents/ap_updater"),
            is_chat_default_only: false,
            capabilities: None,
            enabled_tools: Some(&enabled),
            adapter: PrivateAppAdapterKind::CodexMcpAgent,
            workspace_mcp_paired: None,
            board_active: None,
            server_readiness: None,
        };
        assert_eq!(check_private_app_readiness(&opt_in), Ok(()));

        // AOS-690 server readiness reasons propagate when present.
        let srv = ServerPrivateAppReadiness {
            ready: false,
            actor: None,
            reasons: vec![
                "unpaired_workspace_mcp".into(),
                "missing_write_scope".into(),
            ],
        };
        let mut with_srv = candidate;
        with_srv.server_readiness = Some(&srv);
        assert_eq!(
            check_private_app_readiness(&with_srv),
            Err(vec![
                PrivateAppReadinessReason::UnpairedWorkspaceMcp,
                PrivateAppReadinessReason::MissingWriteScope,
            ])
        );
    }

    #[test]
    fn updater_resolution_follows_priority_and_refuses_silent_fallbacks() {
        let write_caps = vec!["cli.write".into()];
        let explicit = ready_candidate(Some("agents/ap_explicit"), &write_caps);
        let dedicated = ready_candidate(Some("agents/ap_dedicated"), &write_caps);
        let default = ready_candidate(Some("agents/ap_default"), &write_caps);

        let chosen = resolve_private_app_updater(&PrivateAppUpdaterResolution {
            explicit_agent: Some(explicit),
            dedicated_app_updater: Some(dedicated.clone()),
            workspace_default_updater: Some(default.clone()),
            has_chat_default_app_agent: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            chosen,
            ResolvedPrivateAppUpdater {
                source: PrivateAppUpdaterSource::ExplicitAgent,
                actor: "agents/ap_explicit".into(),
            }
        );

        let chosen_dedicated = resolve_private_app_updater(&PrivateAppUpdaterResolution {
            dedicated_app_updater: Some(dedicated),
            workspace_default_updater: Some(default.clone()),
            has_chat_default_app_agent: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            chosen_dedicated.source,
            PrivateAppUpdaterSource::DedicatedAppUpdater
        );

        let chosen_default = resolve_private_app_updater(&PrivateAppUpdaterResolution {
            workspace_default_updater: Some(default),
            has_chat_default_app_agent: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            chosen_default.source,
            PrivateAppUpdaterSource::WorkspaceDefaultUpdater
        );

        // Chat-only default or Personal Assistant without an updater must fail cleanly.
        assert_eq!(
            resolve_private_app_updater(&PrivateAppUpdaterResolution {
                has_chat_default_app_agent: true,
                ..Default::default()
            }),
            Err(vec![PrivateAppReadinessReason::ChatDefaultNotUpdater])
        );
        assert_eq!(
            resolve_private_app_updater(&PrivateAppUpdaterResolution {
                personal_assistant_requested: true,
                has_chat_default_app_agent: true,
                ..Default::default()
            }),
            Err(vec![PrivateAppReadinessReason::PersonalAssistantExcluded])
        );

        let chat_default = ready_candidate(Some("agents/ap_chat"), &write_caps);
        assert_eq!(
            resolve_private_app_updater(&PrivateAppUpdaterResolution {
                dedicated_app_updater: Some(chat_default.clone()),
                has_chat_default_app_agent: true,
                chat_default_actor: Some("agents/ap_chat"),
                ..Default::default()
            }),
            Err(vec![PrivateAppReadinessReason::ChatDefaultNotUpdater])
        );
        assert_eq!(
            resolve_private_app_updater(&PrivateAppUpdaterResolution {
                workspace_default_updater: Some(chat_default),
                has_chat_default_app_agent: true,
                chat_default_actor: Some("agents/ap_chat"),
                ..Default::default()
            }),
            Err(vec![PrivateAppReadinessReason::ChatDefaultNotUpdater])
        );
    }

    #[test]
    fn readiness_fails_closed_on_unknown_server_reason_empty_not_ready_and_actor_mismatch() {
        let write_caps = vec!["cli.write".into()];

        // 1. server.ready == false with an unrecognized reason fails closed with ServerNotReady.
        let unknown_not_ready = ServerPrivateAppReadiness {
            ready: false,
            actor: Some("agents/ap_1".into()),
            reasons: vec!["policy_locked".into()],
        };
        let mut c1 = ready_candidate(Some("agents/ap_1"), &write_caps);
        c1.server_readiness = Some(&unknown_not_ready);
        assert_eq!(
            check_private_app_readiness(&c1),
            Err(vec![PrivateAppReadinessReason::ServerNotReady])
        );

        // 2. server.ready == true with an unrecognized reason still fails closed with ServerNotReady.
        let unknown_when_ready = ServerPrivateAppReadiness {
            ready: true,
            actor: Some("agents/ap_1".into()),
            reasons: vec!["unexpected_restriction".into()],
        };
        let mut c2 = ready_candidate(Some("agents/ap_1"), &write_caps);
        c2.server_readiness = Some(&unknown_when_ready);
        assert_eq!(
            check_private_app_readiness(&c2),
            Err(vec![PrivateAppReadinessReason::ServerNotReady])
        );

        // 3. server.ready == false with empty reasons returns Err([ServerNotReady]).
        let empty_not_ready = ServerPrivateAppReadiness {
            ready: false,
            actor: None,
            reasons: Vec::new(),
        };
        let mut c3 = ready_candidate(Some("agents/ap_1"), &write_caps);
        c3.server_readiness = Some(&empty_not_ready);
        assert_eq!(
            check_private_app_readiness(&c3),
            Err(vec![PrivateAppReadinessReason::ServerNotReady])
        );

        // 4. Local agents/* actor cannot override a server-reported service/* actor.
        let server_pa = ServerPrivateAppReadiness {
            ready: true,
            actor: Some("service/pa_1".into()),
            reasons: Vec::new(),
        };
        let mut c4 = ready_candidate(Some("agents/ap_1"), &write_caps);
        c4.server_readiness = Some(&server_pa);
        assert_eq!(
            check_private_app_readiness(&c4),
            Err(vec![
                PrivateAppReadinessReason::PersonalAssistantExcluded,
                PrivateAppReadinessReason::ActorMismatch,
            ])
        );

        // 5. Disagreement between two agents/* actors returns ActorMismatch.
        let server_other_agent = ServerPrivateAppReadiness {
            ready: true,
            actor: Some("agents/ap_2".into()),
            reasons: Vec::new(),
        };
        let mut c5 = ready_candidate(Some("agents/ap_1"), &write_caps);
        c5.server_readiness = Some(&server_other_agent);
        assert_eq!(
            check_private_app_readiness(&c5),
            Err(vec![PrivateAppReadinessReason::ActorMismatch])
        );
    }

    #[test]
    fn adapter_kind_from_endpoint_adapter_maps_known_and_unsupported() {
        assert_eq!(
            PrivateAppAdapterKind::from_endpoint_adapter(Some("codex/managed")),
            PrivateAppAdapterKind::CodexMcpAgent
        );
        assert_eq!(
            PrivateAppAdapterKind::from_endpoint_adapter(Some("codex/managed-ws")),
            PrivateAppAdapterKind::CodexMcpAgent
        );
        assert_eq!(
            PrivateAppAdapterKind::from_endpoint_adapter(Some("workspace_mcp")),
            PrivateAppAdapterKind::WorkspaceMcp
        );
        assert_eq!(
            PrivateAppAdapterKind::from_endpoint_adapter(Some("workspace_mcp/hosted")),
            PrivateAppAdapterKind::WorkspaceMcp
        );
        assert_eq!(
            PrivateAppAdapterKind::from_endpoint_adapter(None),
            PrivateAppAdapterKind::Unsupported
        );
        assert_eq!(
            PrivateAppAdapterKind::from_endpoint_adapter(Some("claude/managed")),
            PrivateAppAdapterKind::Unsupported
        );
        assert_eq!(
            PrivateAppAdapterKind::from_endpoint_adapter(Some("codex/pty")),
            PrivateAppAdapterKind::Unsupported
        );
    }

    #[test]
    fn readiness_enforces_both_write_sources_and_shared_validators() {
        let caps = vec!["cli.write".into()];
        let no_write_tools = vec!["read_only".into()];
        let mut both = ready_candidate(Some("agents/ap_updater"), &caps);
        both.enabled_tools = Some(&no_write_tools);
        assert_eq!(
            check_private_app_readiness(&both),
            Err(vec![PrivateAppReadinessReason::MissingWriteScope])
        );

        let write_tools = vec!["cadence_writes".into()];
        both.enabled_tools = Some(&write_tools);
        assert_eq!(check_private_app_readiness(&both), Ok(()));

        assert!(valid_package_ref("art-abc_123"));
        assert!(!valid_package_ref("../escape"));
        assert!(valid_sha256_prefixed(&format!("sha256:{}", "a".repeat(64))));
        assert!(!valid_sha256_prefixed(&format!(
            "sha256:{}",
            "A".repeat(64)
        )));
        assert!(valid_install_id("install_app-1"));
        assert!(!valid_install_id("app_1"));
        assert!(valid_generation("c1:g2"));
        assert!(!valid_generation("c1\ng2"));
        assert!(valid_zip_file_name("app-update.zip"));
        assert!(!valid_zip_file_name("../app.zip"));
    }
}
