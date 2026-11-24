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
pub(crate) mod test_helpers {
    pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let mut hex = String::with_capacity(64);
        for b in hasher.finalize() {
            let _ = write!(&mut hex, "{b:02x}");
        }
        hex
    }

    pub(crate) fn spawn_platform_loopback(
        max_requests: usize,
        artifact_id: String,
        digest_hex: String,
        handler: impl Fn(&str, &serde_json::Value) -> (u16, serde_json::Value) + Send + 'static,
    ) -> (String, std::thread::JoinHandle<()>) {
        use serde_json::{json, Value};
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = format!("http://127.0.0.1:{port}");
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(max_requests) {
                let Ok(mut stream) = stream else { break };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let mut header_end = None;
                while header_end.is_none() {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
                }
                let Some(hend) = header_end else { continue };
                let headers = String::from_utf8_lossy(&buf[..hend]);
                let path = headers
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .to_string();
                let content_len = headers
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                while buf.len() < hend + content_len {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let body_bytes = &buf[hend..hend + content_len];
                let (status, resp_json) = match path.as_str() {
                    "/__platform/cli/authorize" => (
                        200,
                        json!({"version": "hosted-cadence-cli.v1", "envelope": "hdr.pay.sig"}),
                    ),
                    "/__platform/wiki/authorize" => (
                        200,
                        json!({"version": "hosted-cadence-wiki.v1", "envelope": "hdr.pay.sig"}),
                    ),
                    "/__platform/artifacts/upload" => (
                        200,
                        json!({"file": {"id": artifact_id, "version": 1, "digest": digest_hex}}),
                    ),
                    _ => {
                        let req: Value = serde_json::from_slice(body_bytes).unwrap_or(json!({}));
                        handler(&path, &req)
                    }
                };
                let body_str = resp_json.to_string();
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
                    body_str.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (endpoint, handle)
    }

    pub(crate) fn review_tool_data(
        tool: &str,
        review_id: &str,
        pkg_digest: &str,
        zip_sha256: &str,
    ) -> serde_json::Value {
        use serde_json::json;
        match tool {
            "private_app_review_request" => json!({
                "id": review_id,
                "state": "pending",
                "outcome": "awaiting_approval",
                "digest": pkg_digest,
                "zip_sha256": zip_sha256,
            }),
            "private_app_review_status" => json!({
                "id": review_id,
                "state": "pending",
                "outcome": "authorized",
            }),
            "private_app_review_apply" | "private_app_review_reconcile" => json!({
                "id": review_id,
                "state": "installed",
                "outcome": "applied",
            }),
            _ => json!({}),
        }
    }

    pub(crate) fn remote_target(endpoint: &str) -> crate::remote_cli::RemoteTarget {
        crate::remote_cli::RemoteTarget {
            org: "acme".into(),
            endpoint: endpoint.into(),
            org_id: "org_1".into(),
            identity: Some(crate::remote_cli::CliIdentityPin {
                subject_id: "sub_12345".into(),
                issuer: "https://auth.example".into(),
            }),
        }
    }

    pub(crate) fn write_agent_slot(
        state_dir: &std::path::Path,
        message_id: &str,
        target: &crate::remote_cli::RemoteTarget,
    ) -> crate::remote_cli::AgentSlot {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};
        let cred_path =
            crate::local_assignment::reporting::agent_credential_path(state_dir, message_id)
                .unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let cred = serde_json::json!({
            "version": "cadence-agent-credential.v1",
            "assignment_id": "hla_test1",
            "claim_id": "hlc_test1",
            "generation": 1,
            "organization_id": "org_1",
            "audience": target.endpoint,
            "capabilities": ["cli.read", "cli.write"],
            "access_token": format!("hct_{}", "a".repeat(43)),
            "expires_at": now + 600,
        });
        std::fs::write(&cred_path, cred.to_string()).unwrap();
        std::fs::set_permissions(&cred_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        crate::remote_cli::AgentSlot::new(
            cred_path,
            target,
            "hla_test1".into(),
            "hlc_test1".into(),
            1,
        )
    }

    pub(crate) fn kickoff<'a>(
        alias: &'a str,
        issue_id: &'a str,
        worktree: &'a str,
        message_id: &'a str,
        endpoint: &'a str,
        profile_id: Option<&'a str>,
    ) -> crate::local_assignment::Kickoff<'a> {
        crate::local_assignment::Kickoff {
            alias,
            issue_id,
            assignment_id: "hla_test1",
            worktree,
            message_id,
            claim_id: "hlc_test1",
            generation: 1,
            organization_id: "org_1",
            audience: endpoint,
            subject_id: Some("sub_12345"),
            issuer: Some("https://auth.example"),
            profile_id,
            action: "dispatch",
            note: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        check_private_app_readiness, resolve_private_app_updater, valid_generation,
        valid_install_id, valid_package_ref, valid_sha256_prefixed, valid_zip_file_name,
        PrivateAppAdapterKind as A, PrivateAppReadinessReason as R, PrivateAppUpdaterCandidate,
        PrivateAppUpdaterResolution, PrivateAppUpdaterSource as S, ResolvedPrivateAppUpdater,
        ServerPrivateAppReadiness,
    };

    fn ready_candidate<'a>(
        actor: Option<&'a str>,
        caps: &'a [String],
    ) -> PrivateAppUpdaterCandidate<'a> {
        PrivateAppUpdaterCandidate {
            actor,
            is_chat_default_only: false,
            capabilities: Some(caps),
            enabled_tools: None,
            adapter: A::CodexMcpAgent,
            workspace_mcp_paired: None,
            board_active: Some(true),
            server_readiness: None,
        }
    }

    #[test]
    fn readiness_excludes_personal_assistant_chat_default_missing_scope_and_unsupported() {
        let wc = vec!["cli.read".into(), "cli.write".into()];
        let rc = vec!["cli.read".into()];
        let (cm, wm, un) = (A::CodexMcpAgent, A::WorkspaceMcp, A::Unsupported);
        let chk = |actor, write, chat, adapter, paired, exp| {
            let mut c = ready_candidate(Some(actor), if write { &wc } else { &rc });
            c.is_chat_default_only = chat;
            c.adapter = adapter;
            c.workspace_mcp_paired = paired;
            assert_eq!(check_private_app_readiness(&c), Err(vec![exp]));
        };
        chk(
            "service/pa_1",
            true,
            false,
            cm,
            None,
            R::PersonalAssistantExcluded,
        );
        chk(
            "agents/ap_chat",
            true,
            true,
            cm,
            None,
            R::ChatDefaultNotUpdater,
        );
        chk("agents/ap_1", false, false, cm, None, R::MissingWriteScope);
        chk(
            "agents/ap_1",
            true,
            false,
            un,
            None,
            R::AdapterUploadUnsupported,
        );
        chk(
            "agents/ap_1",
            true,
            false,
            wm,
            Some(false),
            R::UnpairedWorkspaceMcp,
        );
    }

    #[test]
    fn readiness_accepts_codex_and_degrades_cleanly_when_server_readiness_absent() {
        let wc = vec!["cli.write".into()];
        let candidate = ready_candidate(Some("agents/ap_updater"), &wc);
        assert_eq!(check_private_app_readiness(&candidate), Ok(()));

        // CAD-2561 opt-in profile flag without explicit token capabilities still validates.
        let enabled = vec!["cadence_writes".into()];
        let mut opt_in = ready_candidate(Some("agents/ap_updater"), &wc);
        opt_in.capabilities = None;
        opt_in.enabled_tools = Some(&enabled);
        opt_in.board_active = None;
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
            Err(vec![R::UnpairedWorkspaceMcp, R::MissingWriteScope])
        );
    }

    #[test]
    fn updater_resolution_follows_priority_and_refuses_silent_fallbacks() {
        let wc = vec!["cli.write".into()];
        let r = |e: Option<&str>, d: Option<&str>, w: Option<&str>, pa: bool, c: Option<&str>| {
            resolve_private_app_updater(&PrivateAppUpdaterResolution {
                explicit_agent: e.map(|a| ready_candidate(Some(a), &wc)),
                dedicated_app_updater: d.map(|a| ready_candidate(Some(a), &wc)),
                workspace_default_updater: w.map(|a| ready_candidate(Some(a), &wc)),
                has_chat_default_app_agent: true,
                personal_assistant_requested: pa,
                chat_default_actor: c,
            })
        };
        let (ex, dd, df) = (
            Some("agents/ap_explicit"),
            Some("agents/ap_dedicated"),
            Some("agents/ap_default"),
        );
        let ch = Some("agents/ap_chat");

        assert_eq!(
            r(ex, dd, df, false, None).unwrap(),
            ResolvedPrivateAppUpdater {
                source: S::ExplicitAgent,
                actor: "agents/ap_explicit".into()
            }
        );
        assert_eq!(
            r(None, dd, df, false, None).unwrap().source,
            S::DedicatedAppUpdater
        );
        assert_eq!(
            r(None, None, df, false, None).unwrap().source,
            S::WorkspaceDefaultUpdater
        );
        // Chat-only default, Personal Assistant, or chat-default in updater slots fail cleanly.
        assert_eq!(
            r(None, None, None, false, None).unwrap_err(),
            [R::ChatDefaultNotUpdater]
        );
        assert_eq!(
            r(None, None, None, true, None).unwrap_err(),
            [R::PersonalAssistantExcluded]
        );
        assert_eq!(
            r(None, ch, None, false, ch).unwrap_err(),
            [R::ChatDefaultNotUpdater]
        );
        assert_eq!(
            r(None, None, ch, false, ch).unwrap_err(),
            [R::ChatDefaultNotUpdater]
        );
    }

    #[test]
    fn readiness_fails_closed_on_unknown_server_reason_empty_not_ready_and_actor_mismatch() {
        let wc = vec!["cli.write".into()];
        let chk = |ready, actor: Option<&str>, reasons: &[&str], exp: &[R]| {
            let srv = ServerPrivateAppReadiness {
                ready,
                actor: actor.map(str::to_string),
                reasons: reasons.iter().map(|s| (*s).into()).collect(),
            };
            let mut c = ready_candidate(Some("agents/ap_1"), &wc);
            c.server_readiness = Some(&srv);
            assert_eq!(check_private_app_readiness(&c), Err(exp.to_vec()));
        };
        let (a1, a2, pa) = (
            Some("agents/ap_1"),
            Some("agents/ap_2"),
            Some("service/pa_1"),
        );
        chk(false, a1, &["policy_locked"], &[R::ServerNotReady]);
        chk(true, a1, &["unexpected_restriction"], &[R::ServerNotReady]);
        chk(false, None, &[], &[R::ServerNotReady]);
        chk(
            true,
            pa,
            &[],
            &[R::PersonalAssistantExcluded, R::ActorMismatch],
        );
        chk(true, a2, &[], &[R::ActorMismatch]);
    }

    #[test]
    fn adapter_kind_from_endpoint_adapter_maps_known_and_unsupported() {
        let chk = |i, e| assert_eq!(A::from_endpoint_adapter(i), e);
        chk(Some("codex/managed"), A::CodexMcpAgent);
        chk(Some("codex/managed-ws"), A::CodexMcpAgent);
        chk(Some("workspace_mcp"), A::WorkspaceMcp);
        chk(Some("workspace_mcp/hosted"), A::WorkspaceMcp);
        chk(None, A::Unsupported);
        chk(Some("claude/managed"), A::Unsupported);
        chk(Some("codex/pty"), A::Unsupported);
    }

    #[test]
    fn readiness_enforces_both_write_sources_and_shared_validators() {
        let caps = vec!["cli.write".into()];
        let no_write_tools = vec!["read_only".into()];
        let mut both = ready_candidate(Some("agents/ap_updater"), &caps);
        both.enabled_tools = Some(&no_write_tools);
        assert_eq!(
            check_private_app_readiness(&both),
            Err(vec![R::MissingWriteScope])
        );

        let write_tools = vec!["cadence_writes".into()];
        both.enabled_tools = Some(&write_tools);
        assert_eq!(check_private_app_readiness(&both), Ok(()));

        assert!(valid_package_ref("art-abc_123") && !valid_package_ref("../escape"));
        assert!(
            valid_sha256_prefixed(&format!("sha256:{}", "a".repeat(64)))
                && !valid_sha256_prefixed(&format!("sha256:{}", "A".repeat(64)))
        );
        assert!(valid_install_id("install_app-1") && !valid_install_id("app_1"));
        assert!(valid_generation("c1:g2") && !valid_generation("c1\ng2"));
        assert!(valid_zip_file_name("app-update.zip") && !valid_zip_file_name("../app.zip"));
    }
}
