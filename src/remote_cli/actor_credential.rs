//! CAD-2747 (client half of AOS-730): default unmanaged external harness CLI
//! calls to the org user's personal assistant, with explicit `--agent`
//! specialization and `--as-human` escape hatch.
//!
//! Managed daemon panes (`CADENCE_ALIAS`) never use this module: they remain
//! bound to [`super::agent_credential`].

use super::*;
use crate::remote_enrollment::cli_session_post_detailed;
use std::sync::Mutex;

const VERSION: &str = "hosted-cadence-cli-session.v1";
const FILE_VERSION: &str = "cadence-cli-actor-credential.v1";
/// Maximum actor credential lifetime allowed by AOS-730 (`15 * 60s`), plus skew.
const MAX_LIFETIME: u64 = 900 + SKEW;

/// Non-empty environment variables set by external interactive/CLI coding harnesses.
const EXTERNAL_HARNESS_ENV_VARS: &[&str] = &[
    "ANTIGRAVITY_AGENT",
    "ANTIGRAVITY_CONVERSATION_ID",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "OMP_SESSION",
    "OMP_AGENT",
    "PI_CODING_AGENT",
    "PI_SESSION",
    "CURSOR_AGENT",
    "OPENCODE",
    "AIDER",
    "DEVIN",
    "AGENTICOS_HARNESS",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliActorMode {
    Assistant,
    Agent(String),
    Human,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliActor {
    pub mode: CliActorMode,
}

static BOUND_ACTOR: Mutex<Option<CliActor>> = Mutex::new(None);

/// Record the active unmanaged CLI actor override for the current invocation.
pub fn bind_cli_actor(actor: Option<CliActor>) {
    *BOUND_ACTOR
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = actor;
}

/// Test seam: reset the unmanaged CLI actor override.
#[doc(hidden)]
#[cfg(any(debug_assertions, feature = "test-seam"))]
pub fn unbind_cli_actor() {
    *BOUND_ACTOR
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
}

pub fn cli_actor() -> Option<CliActor> {
    BOUND_ACTOR
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// Whether the active unmanaged CLI actor explicitly restricts out human/owner authority.
pub fn cli_actor_is_non_human() -> bool {
    matches!(
        cli_actor().map(|actor| actor.mode),
        Some(CliActorMode::Assistant | CliActorMode::Agent(_))
    )
}

/// Validate a workspace agent profile id passed to `--agent <profileId>`.
pub fn valid_profile_id(value: &str) -> bool {
    value.starts_with("ap_")
        && (4..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether process environment markers indicate an unmanaged external agent harness.
pub fn is_external_harness_terminal() -> bool {
    EXTERNAL_HARNESS_ENV_VARS.iter().any(|key| {
        std::env::var_os(key).is_some_and(|val| !val.to_string_lossy().trim().is_empty())
    })
}

/// Resolve the active unmanaged actor mode from CLI flags and harness env vars.
pub fn detect_unmanaged_actor(agent: Option<&str>, as_human: bool) -> Result<Option<CliActor>> {
    if let Some(profile_id) = agent {
        if !valid_profile_id(profile_id) {
            return Err(Error::usage(
                "--agent requires a valid workspace agent profile id (for example `ap_...`)",
            ));
        }
        return Ok(Some(CliActor {
            mode: CliActorMode::Agent(profile_id.to_owned()),
        }));
    }
    if as_human {
        return Ok(Some(CliActor {
            mode: CliActorMode::Human,
        }));
    }
    if let Ok(raw) = std::env::var("CADENCE_CLI_ACTOR") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return match trimmed {
                "assistant" => Ok(Some(CliActor {
                    mode: CliActorMode::Assistant,
                })),
                "human" => Ok(Some(CliActor {
                    mode: CliActorMode::Human,
                })),
                _ => Err(Error::usage(
                    "CADENCE_CLI_ACTOR must be `assistant` or `human`",
                )),
            };
        }
    }
    if is_external_harness_terminal() {
        return Ok(Some(CliActor {
            mode: CliActorMode::Assistant,
        }));
    }
    Ok(None)
}

/// Best-effort check whether the stored login for `target` was issued with
/// `actor_mode: "assistant"` (via `cadence login --as-assistant`).
pub fn saved_login_is_assistant_only(target: &RemoteTarget) -> bool {
    let Ok(dir) = crate::remote_auth::auth_dir(None) else {
        return false;
    };
    let Ok(cred) = read_credential_metadata(&dir, target) else {
        return false;
    };
    cred.cli_session
        .as_ref()
        .and_then(|session| session.actor_mode.as_deref())
        == Some("assistant")
}

/// Whether `verb` + optional `operation` require human/owner authority and must
/// refuse on assistant or agent CLI sessions rather than impersonating the
/// human owner.
pub(super) fn is_human_only_verb(verb: &str, operation: Option<&str>) -> bool {
    if matches!(
        verb,
        "settings_agents"
            | "master_status"
            | "master_resume"
            | "project_new"
            | "audit_approve"
            | "audit_approve_project"
            | "audit_approve_scope_project"
            | "audit_approve_delegated_project"
            | "audit_reviewers_set"
            | "audit_identities_set"
            | "audit_designate"
            | "audit_revoke"
            | "audit_accept_revise"
            | "delivery_enqueue"
            | "issue_project_delivery"
            | "team_list"
            | "team_invites"
            | "team_invite"
            | "team_revoke_invite"
            | "team_set_role"
            | "team_remove"
            | "agent_profile_create"
            | "agent_profile_update"
            | "agent_profile_delete"
            | "agent_profile_set_role"
    ) {
        return true;
    }
    if verb == "platform_call" {
        if let Some(op) = operation {
            return op.starts_with("connection_");
        }
    }
    false
}

pub(super) enum ActorSource {
    HumanLogin,
    Cached(PathBuf),
    Minted,
}

impl ActorSource {
    pub(super) fn is_cached(&self) -> bool {
        matches!(self, Self::Cached(_))
    }

    pub(super) fn invalidate_cached(&self) {
        if let Self::Cached(path) = self {
            let _ = fs::remove_file(path);
        }
    }
}

/// Resolve the bearer credential for an unmanaged hosted CLI call.
///
/// Never silently falls back to the human login credential when an assistant or
/// `--agent` credential is required or refused.
pub(super) fn bearer_for(
    target: &RemoteTarget,
    dir: &Path,
    deadline: Instant,
    verb: &str,
    operation: Option<&str>,
) -> Result<(Credential, ActorSource)> {
    bearer_for_with(
        target,
        dir,
        deadline,
        verb,
        operation,
        cli_session_post_detailed,
    )
}

fn bearer_for_with(
    target: &RemoteTarget,
    dir: &Path,
    deadline: Instant,
    verb: &str,
    operation: Option<&str>,
    post: impl FnOnce(
        &Path,
        &str,
        &str,
        &str,
        Value,
        Instant,
    ) -> Result<crate::remote_enrollment::IssuerAnswer>,
) -> Result<(Credential, ActorSource)> {
    let login = renewed_credential(dir, target, deadline)?;
    let login_actor_mode = login
        .cli_session
        .as_ref()
        .and_then(|session| session.actor_mode.as_deref());
    let assistant_login = login_actor_mode == Some("assistant");
    let actor = cli_actor();

    if matches!(actor.as_ref().map(|a| &a.mode), Some(CliActorMode::Human)) {
        if assistant_login {
            return Err(reject(format!(
                "saved CLI login for org '{}' is assistant-only (`actor_mode: assistant`); \
                 `--as-human` requires a human login via `cadence login`. Nothing was sent",
                target.org
            )));
        }
        return Ok((login, ActorSource::HumanLogin));
    }

    let effective_mode = match actor.map(|a| a.mode) {
        Some(CliActorMode::Agent(profile_id)) => Some(CliActorMode::Agent(profile_id)),
        Some(CliActorMode::Assistant) => Some(CliActorMode::Assistant),
        Some(CliActorMode::Human) => None,
        None if assistant_login => Some(CliActorMode::Assistant),
        None => None,
    };

    let Some(mode) = effective_mode else {
        return Ok((login, ActorSource::HumanLogin));
    };

    if is_human_only_verb(verb, operation) {
        let remedy = if assistant_login {
            "re-login via `cadence login` from an authorized human shell"
        } else {
            "re-run explicitly with `--as-human` from an authorized human shell"
        };
        return Err(reject(format!(
            "remote `{verb}` requires human/owner authority and is refused in assistant/agent \
             CLI mode — {remedy}. Nothing was sent"
        )));
    }

    let path = cache_path(dir, target, &mode)?;
    let session_id = login
        .cli_session
        .as_ref()
        .map(|session| session.session_id.as_str())
        .ok_or_else(|| {
            reject(
                "assistant/agent CLI mode requires a renewable CLI session — run `cadence login` \
                 again (or pass `--as-human` on a plain human shell); nothing was sent",
            )
        })?;

    let at = now()?;
    if let Some(cached) = read_cached(&path, target, &mode, session_id, at) {
        return Ok((cached, ActorSource::Cached(path)));
    }

    let cred = mint_with(&login, target, dir, deadline, &mode, post)?;
    let _ = write_cache(&path, target, &mode, session_id, &cred);
    Ok((cred, ActorSource::Minted))
}

fn cache_path(dir: &Path, target: &RemoteTarget, mode: &CliActorMode) -> Result<PathBuf> {
    let filename = match mode {
        CliActorMode::Assistant => format!("actor-{}-assistant.json", target.org),
        CliActorMode::Agent(profile_id) => {
            if !valid_profile_id(profile_id) {
                return Err(reject("invalid agent profile id"));
            }
            format!("actor-{}-agent-{profile_id}.json", target.org)
        }
        CliActorMode::Human => return Err(reject("human mode does not cache an actor credential")),
    };
    Ok(dir.join(filename))
}

fn mint_with(
    login: &Credential,
    target: &RemoteTarget,
    dir: &Path,
    deadline: Instant,
    mode: &CliActorMode,
    post: impl FnOnce(
        &Path,
        &str,
        &str,
        &str,
        Value,
        Instant,
    ) -> Result<crate::remote_enrollment::IssuerAnswer>,
) -> Result<Credential> {
    let issuer = login.issuer.as_deref().ok_or_else(|| {
        reject("hosted session issuer missing — run `cadence login` again; nothing was sent")
    })?;
    let body = match mode {
        CliActorMode::Assistant => serde_json::json!({
            "version": VERSION,
            "organization_id": target.org_id,
            "audience": target.endpoint,
            "mode": "assistant",
        }),
        CliActorMode::Agent(profile_id) => serde_json::json!({
            "version": VERSION,
            "organization_id": target.org_id,
            "audience": target.endpoint,
            "mode": "agent",
            "profile_id": profile_id,
        }),
        CliActorMode::Human => return Err(reject("human mode does not mint an actor credential")),
    };
    let request_deadline = deadline.min(Instant::now() + AUTH_TIMEOUT);
    let answer = post(
        dir,
        issuer,
        "/v1/hosted-cadence/cli-session/actor-credential",
        &login.access_token,
        body,
        request_deadline,
    )
    .map_err(|error| {
        reject(format!(
            "could not obtain CLI actor credential ({error}); nothing was sent, and your human \
             login was not used in its place"
        ))
    })?;
    let at = now()?;
    if answer.status != 200 {
        let code = answer.body.get("error").and_then(Value::as_str);
        return Err(reject(format!(
            "hosted issuer refused CLI actor credential (HTTP {}{}); nothing was sent, and your \
             human login was not used in its place",
            answer.status,
            code.map_or(String::new(), |c| format!(", {c}"))
        )));
    }
    validated(target, mode, &answer.body, at)
}

fn validated(
    target: &RemoteTarget,
    mode: &CliActorMode,
    answer: &Value,
    at: u64,
) -> Result<Credential> {
    let unusable =
        || reject("the issuer's CLI actor credential response was invalid; nothing was sent");
    let text = |pointer: &str| answer.pointer(pointer).and_then(Value::as_str);
    let credential = answer.get("credential").ok_or_else(unusable)?;
    let token = credential.get("access_token").and_then(Value::as_str);
    let expires_at = credential.get("expires_at").and_then(Value::as_u64);
    let issued_at = credential.get("issued_at").and_then(Value::as_u64);
    let capabilities: Option<Vec<String>> = answer
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        });
    let (Some(token), Some(expires_at), Some(issued_at), Some(capabilities)) =
        (token, expires_at, issued_at, capabilities)
    else {
        return Err(unusable());
    };
    let principal_ok = match mode {
        CliActorMode::Assistant => {
            text("/principal/kind") == Some("service")
                && text("/principal/assistant_id").is_some_and(|s| !s.is_empty())
                && text("/principal/actor").is_some_and(|s| s.starts_with("assistant:"))
                && text("/principal/role_at_issue") == Some("member")
        }
        CliActorMode::Agent(profile_id) => {
            text("/principal/kind") == Some("agent")
                && text("/principal/profile_id") == Some(profile_id.as_str())
                && text("/principal/actor") == Some(format!("agents/{profile_id}").as_str())
                && text("/principal/role_at_issue") == Some("member")
        }
        CliActorMode::Human => false,
    };
    if text("/version") != Some(VERSION)
        || text("/organization_id") != Some(target.org_id.as_str())
        || text("/audience") != Some(target.endpoint.as_str())
        || !principal_ok
        || credential.get("token_type").and_then(Value::as_str) != Some("Bearer")
        || credential.get("renewal").and_then(Value::as_str) != Some("remint")
        || !valid_token(token, "hct_")
        || issued_at > at.saturating_add(SKEW)
        || expires_at <= at.saturating_add(SKEW)
        || expires_at > issued_at.saturating_add(MAX_LIFETIME)
        || capabilities.is_empty()
    {
        return Err(unusable());
    }
    Ok(credential_from(
        target,
        token.to_owned(),
        capabilities,
        expires_at,
    ))
}

fn credential_from(
    target: &RemoteTarget,
    token: String,
    capabilities: Vec<String>,
    expires_at: u64,
) -> Credential {
    Credential {
        organization_id: target.org_id.clone(),
        organization_slug: Some(target.org.clone()),
        audience: target.endpoint.clone(),
        issuer: target.identity.as_ref().map(|pin| pin.issuer.clone()),
        subject_id: target.identity.as_ref().map(|pin| pin.subject_id.clone()),
        capabilities,
        access_token: token,
        expires_at,
        cli_session: None,
        pending: None,
    }
}

fn mode_label(mode: &CliActorMode) -> &'static str {
    match mode {
        CliActorMode::Assistant => "assistant",
        CliActorMode::Agent(_) => "agent",
        CliActorMode::Human => "human",
    }
}

fn mode_profile(mode: &CliActorMode) -> Option<&str> {
    match mode {
        CliActorMode::Agent(profile_id) => Some(profile_id.as_str()),
        CliActorMode::Assistant | CliActorMode::Human => None,
    }
}

fn read_cached(
    path: &Path,
    target: &RemoteTarget,
    mode: &CliActorMode,
    session_id: &str,
    at: u64,
) -> Option<Credential> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.len() > 8192
    {
        return None;
    }
    let mut raw = Vec::new();
    file.take(8193).read_to_end(&mut raw).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let token = text("access_token")?;
    let expires_at = value.get("expires_at").and_then(Value::as_u64)?;
    let capabilities: Vec<String> = value
        .get("capabilities")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    if text("version") != Some(FILE_VERSION)
        || text("organization_id") != Some(target.org_id.as_str())
        || text("audience") != Some(target.endpoint.as_str())
        || text("session_id") != Some(session_id)
        || text("mode") != Some(mode_label(mode))
        || text("profile_id") != mode_profile(mode)
        || !valid_token(token, "hct_")
        || expires_at <= at.saturating_add(RENEW_BEFORE)
        || expires_at > at.saturating_add(MAX_LIFETIME)
        || capabilities.is_empty()
    {
        return None;
    }
    Some(credential_from(
        target,
        token.to_owned(),
        capabilities,
        expires_at,
    ))
}

fn write_cache(
    path: &Path,
    target: &RemoteTarget,
    mode: &CliActorMode,
    session_id: &str,
    cred: &Credential,
) -> Result<()> {
    private_directory(path.parent().unwrap_or(Path::new(".")))?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| reject("unable to create actor cache path"))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("actor");
    let tmp = path.with_file_name(format!(
        ".{name}.tmp-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random)
    ));
    let body = serde_json::json!({
        "version": FILE_VERSION,
        "organization_id": target.org_id,
        "audience": target.endpoint,
        "session_id": session_id,
        "mode": mode_label(mode),
        "profile_id": mode_profile(mode),
        "capabilities": cred.capabilities,
        "access_token": cred.access_token,
        "expires_at": cred.expires_at,
    });
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)?;
    let result = (|| {
        file.write_all(
            &serde_json::to_vec(&body).map_err(|_| reject("cannot encode actor cache"))?,
        )?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(String, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn clear_harness() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let mut saved = Vec::new();
            for key in EXTERNAL_HARNESS_ENV_VARS
                .iter()
                .copied()
                .chain(std::iter::once("CADENCE_CLI_ACTOR"))
            {
                saved.push((key.to_string(), std::env::var_os(key)));
                unsafe { std::env::remove_var(key) };
            }
            Self { _lock: lock, saved }
        }

        fn set(&mut self, key: &str, value: &str) {
            if !self.saved.iter().any(|(k, _)| k == key) {
                self.saved.push((key.to_string(), std::env::var_os(key)));
            }
            unsafe { std::env::set_var(key, value) };
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unbind_cli_actor();
            for (key, value) in self.saved.drain(..) {
                match value {
                    Some(v) => unsafe { std::env::set_var(&key, v) },
                    None => unsafe { std::env::remove_var(&key) },
                }
            }
        }
    }

    #[test]
    fn external_harness_defaults_to_assistant_unless_overridden() {
        let mut guard = EnvGuard::clear_harness();
        assert_eq!(detect_unmanaged_actor(None, false).unwrap(), None);

        guard.set("CLAUDECODE", "1");
        assert_eq!(
            detect_unmanaged_actor(None, false).unwrap().map(|a| a.mode),
            Some(CliActorMode::Assistant)
        );
        assert_eq!(
            detect_unmanaged_actor(None, true).unwrap().map(|a| a.mode),
            Some(CliActorMode::Human)
        );
        assert_eq!(
            detect_unmanaged_actor(Some("ap_reviewer-1"), false)
                .unwrap()
                .map(|a| a.mode),
            Some(CliActorMode::Agent("ap_reviewer-1".to_owned()))
        );
        guard.set("CADENCE_CLI_ACTOR", "human");
        assert_eq!(
            detect_unmanaged_actor(None, false).unwrap().map(|a| a.mode),
            Some(CliActorMode::Human)
        );
        guard.set("CADENCE_CLI_ACTOR", "bogus");
        assert!(detect_unmanaged_actor(None, false).is_err());
    }

    #[test]
    fn profile_id_validation_rejects_traversal_and_invalid_prefixes() {
        assert!(valid_profile_id("ap_2ca20330-f4db-4195-bed2-e5e4e21b5787"));
        assert!(!valid_profile_id("ap_"));
        assert!(!valid_profile_id("../etc/passwd"));
        assert!(!valid_profile_id("usr_1234"));
    }

    #[test]
    fn human_only_verbs_cover_owner_surfaces_and_connections() {
        assert!(is_human_only_verb("settings_agents", None));
        assert!(is_human_only_verb("audit_approve", None));
        assert!(is_human_only_verb("delivery_enqueue", None));
        assert!(is_human_only_verb("team_invite", None));
        assert!(is_human_only_verb(
            "platform_call",
            Some("connection_start")
        ));
        assert!(!is_human_only_verb("issue_show", None));
        assert!(!is_human_only_verb("platform_call", Some("call")));
    }

    #[test]
    fn refuses_as_human_when_saved_login_is_assistant_only() {
        let _guard = EnvGuard::clear_harness();
        let dir = std::env::temp_dir().join(format!("cad2747-asst-login-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let target = RemoteTarget {
            org: "acme".into(),
            endpoint: "https://acme.cadencecloud.app".into(),
            org_id: "ws_acme".into(),
            identity: None,
        };
        let at = now().unwrap();
        let session = super::CliSession {
            session_id: format!("hcs_{}", "0".repeat(36)),
            refresh_token: format!("hcr_{}", "r".repeat(43)),
            generation: 1,
            device_label: "cli".into(),
            idle_expires_at: at + 3600,
            absolute_expires_at: at + 86400,
            actor_mode: Some("assistant".into()),
        };
        let login = Credential {
            organization_id: target.org_id.clone(),
            organization_slug: Some(target.org.clone()),
            audience: target.endpoint.clone(),
            issuer: Some("https://issuer.invalid".into()),
            subject_id: Some("usr_1".into()),
            capabilities: vec!["cli.read".into(), "cli.write".into()],
            access_token: format!("hct_{}", "l".repeat(43)),
            expires_at: at + 300,
            cli_session: Some(session),
            pending: None,
        };
        super::save_credential(&dir, &target.org, &login).unwrap();

        bind_cli_actor(Some(CliActor {
            mode: CliActorMode::Human,
        }));
        let err = bearer_for(
            &target,
            &dir,
            Instant::now() + AUTH_TIMEOUT,
            "issue_list",
            None,
        )
        .err()
        .expect("assistant-only login must refuse --as-human");
        assert!(err.to_string().contains("assistant-only"), "{err}");

        bind_cli_actor(None);
        let err_settings = bearer_for(
            &target,
            &dir,
            Instant::now() + AUTH_TIMEOUT,
            "settings_agents",
            None,
        )
        .err()
        .expect("assistant-only login must refuse human-only verbs");
        assert!(
            err_settings.to_string().contains("human/owner authority"),
            "{err_settings}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mints_and_caches_assistant_and_agent_credentials_without_human_fallback() {
        let _guard = EnvGuard::clear_harness();
        let issuer = "https://issuer.invalid";
        let dir = std::env::temp_dir().join(format!("cad2747-mint-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let target = RemoteTarget {
            org: "acme".into(),
            endpoint: "https://acme.cadencecloud.app".into(),
            org_id: "ws_acme".into(),
            identity: None,
        };
        let at = now().unwrap();
        let session = super::CliSession {
            session_id: format!("hcs_{}", "1".repeat(36)),
            refresh_token: format!("hcr_{}", "r".repeat(43)),
            generation: 1,
            device_label: "cli".into(),
            idle_expires_at: at + 3600,
            absolute_expires_at: at + 86400,
            actor_mode: Some("human".into()),
        };
        let login = Credential {
            organization_id: target.org_id.clone(),
            organization_slug: Some(target.org.clone()),
            audience: target.endpoint.clone(),
            issuer: Some(issuer.into()),
            subject_id: Some("usr_1".into()),
            capabilities: vec![
                "cli.read".into(),
                "cli.write".into(),
                "wiki.read".into(),
                "wiki.write".into(),
            ],
            access_token: format!("hct_{}", "l".repeat(43)),
            expires_at: at + 300,
            cli_session: Some(session),
            pending: None,
        };
        super::save_credential(&dir, &target.org, &login).unwrap();

        let assistant_token = format!("hct_{}", "a".repeat(43));
        let agent_token = format!("hct_{}", "g".repeat(43));
        let posts = std::cell::Cell::new(0usize);

        bind_cli_actor(Some(CliActor {
            mode: CliActorMode::Assistant,
        }));
        let (cred1, src1) = bearer_for_with(
            &target,
            &dir,
            Instant::now() + AUTH_TIMEOUT,
            "issue_list",
            None,
            |_, iss, path, bearer, body, _| {
                posts.set(posts.get() + 1);
                assert_eq!(iss, issuer);
                assert_eq!(path, "/v1/hosted-cadence/cli-session/actor-credential");
                assert_eq!(bearer, login.access_token);
                assert_eq!(body["mode"], "assistant");
                Ok(crate::remote_enrollment::IssuerAnswer {
                    status: 200,
                    body: serde_json::json!({
                        "version": VERSION,
                        "organization_id": "ws_acme",
                        "audience": "https://acme.cadencecloud.app",
                        "principal": {
                            "kind": "service",
                            "assistant_id": "ast_123",
                            "actor": "assistant:ast_123",
                            "role_at_issue": "member",
                        },
                        "capabilities": ["cli.read", "cli.write", "wiki.read", "wiki.write"],
                        "credential": {
                            "credential_id": "hcp_1",
                            "access_token": assistant_token,
                            "token_type": "Bearer",
                            "issued_at": at,
                            "expires_at": at + 600,
                            "renewal": "remint",
                        }
                    }),
                    correlation: None,
                })
            },
        )
        .unwrap();
        assert!(matches!(src1, ActorSource::Minted));
        assert_eq!(cred1.access_token, assistant_token);

        let (cred2, src2) = bearer_for_with(
            &target,
            &dir,
            Instant::now() + AUTH_TIMEOUT,
            "issue_list",
            None,
            |_, _, _, _, _, _| panic!("cached actor credential must not mint again"),
        )
        .unwrap();
        assert!(src2.is_cached());
        assert_eq!(cred2.access_token, assistant_token);

        bind_cli_actor(Some(CliActor {
            mode: CliActorMode::Agent("ap_dev".into()),
        }));
        let (cred3, src3) = bearer_for_with(
            &target,
            &dir,
            Instant::now() + AUTH_TIMEOUT,
            "issue_list",
            None,
            |_, _, path, _, body, _| {
                posts.set(posts.get() + 1);
                assert_eq!(path, "/v1/hosted-cadence/cli-session/actor-credential");
                assert_eq!(body["mode"], "agent");
                assert_eq!(body["profile_id"], "ap_dev");
                Ok(crate::remote_enrollment::IssuerAnswer {
                    status: 200,
                    body: serde_json::json!({
                        "version": VERSION,
                        "organization_id": "ws_acme",
                        "audience": "https://acme.cadencecloud.app",
                        "principal": {
                            "kind": "agent",
                            "profile_id": "ap_dev",
                            "subject_id": "agent_ap_dev",
                            "actor": "agents/ap_dev",
                            "role_at_issue": "member",
                        },
                        "capabilities": ["cli.read", "cli.write"],
                        "credential": {
                            "credential_id": "hca_1",
                            "access_token": agent_token,
                            "token_type": "Bearer",
                            "issued_at": at,
                            "expires_at": at + 600,
                            "renewal": "remint",
                        }
                    }),
                    correlation: None,
                })
            },
        )
        .unwrap();
        assert!(matches!(src3, ActorSource::Minted));
        assert_eq!(cred3.access_token, agent_token);

        bind_cli_actor(Some(CliActor {
            mode: CliActorMode::Agent("ap_denied".into()),
        }));
        let err = bearer_for_with(
            &target,
            &dir,
            Instant::now() + AUTH_TIMEOUT,
            "issue_list",
            None,
            |_, _, _, _, _, _| {
                posts.set(posts.get() + 1);
                Ok(crate::remote_enrollment::IssuerAnswer {
                    status: 403,
                    body: serde_json::json!({ "error": "forbidden" }),
                    correlation: None,
                })
            },
        )
        .err()
        .expect("403 mint refusal must never fall back to human login");
        assert!(
            err.to_string().contains("human login was not used"),
            "{err}"
        );
        assert_eq!(posts.get(), 3);

        let _ = fs::remove_dir_all(&dir);
    }
}
