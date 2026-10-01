//! CAD-1002: provider launch options and the launch/join path — moved verbatim
//! from `cli/mod.rs` (CAD-984 PR-2).

use super::*;

/// Claude-specific launch options — stored under `params` so the
/// adapter replays them verbatim on every resume (`--permission-mode`,
/// `--allowedTools`, `--model`).
#[derive(Default)]
pub(crate) struct ClaudeOpts {
    pub(super) model: Option<String>,
    /// `params.effort` — the CLI's `--effort` level.
    pub(super) effort: Option<String>,
    pub(super) permission_mode: Option<String>,
    pub(super) allow: Vec<String>,
    pub(super) bypass: bool,
    /// `params.broker_approvals` — route permission prompts to
    /// `agent requests`/`agent respond` via the mcp-permission server.
    pub(super) broker_approvals: bool,
    /// `params.permission_timeout_secs` — operator-decision deadline
    /// before a brokered prompt denies (default 900).
    pub(super) permission_timeout_secs: Option<u64>,
    /// `params.turn_idle_secs` — inactivity window before a turn is
    /// `unknown` (activity-based liveness; default 900).
    pub(super) turn_idle_secs: Option<u64>,
    /// `params.turn_max_secs` — optional absolute turn cap.
    pub(super) turn_max_secs: Option<u64>,
    /// `params.confine` (CAD-556) — pi only: `Some(true)` from
    /// `--confine`, `Some(false)` from `--no-confine`, `None` defers
    /// to the pm.yaml `[host] confine_pi_workers` default.
    pub(super) confine: Option<bool>,
}

/// Devin-specific launch options. Pty stores `permission_mode` so the
/// same argv replays on every pane open. `--cloud` stores the v3
/// create params instead; `--bypass` is the pty `dangerous` shorthand
/// and is refused for a cloud session.
#[derive(Default)]
pub(crate) struct DevinOpts {
    pub(super) permission_mode: Option<String>,
    pub(super) bypass: bool,
    pub(super) cloud: bool,
    pub(super) repos: Vec<String>,
    pub(super) devin_mode: Option<String>,
    pub(super) max_acu_limit: Option<u64>,
    pub(super) playbook_id: Option<String>,
    pub(super) knowledge_ids: Vec<String>,
    pub(super) secret_ids: Vec<String>,
    pub(super) platform: Option<String>,
    pub(super) tags: Vec<String>,
    pub(super) bypass_approval: bool,
    pub(super) attachment_urls: Vec<String>,
}

/// Cursor-specific launch options — `params.model` and
/// `params.permission_mode` ride the profile so the same
/// `--model`/`--force`/`--auto-review` argv replays on every pane open.
/// `--bypass` is the `force` shorthand.
#[derive(Default)]
pub(crate) struct CursorOpts {
    pub(super) model: Option<String>,
    pub(super) permission_mode: Option<String>,
    pub(super) bypass: bool,
}

/// Codex app-server launch settings. They are stored in the agent params so
/// the adapter can replay the same model and reasoning effort on resume.
#[derive(Default)]
pub(crate) struct CodexOpts {
    pub(super) model: Option<String>,
    pub(super) effort: Option<String>,
    /// `params.approval_policy`, replayed on every open.
    pub(super) approval_policy: Option<String>,
    /// `params.turn_idle_secs` / `params.turn_max_secs` — the same
    /// activity-based turn liveness as managed claude (CAD-227).
    pub(super) turn_idle_secs: Option<u64>,
    pub(super) turn_max_secs: Option<u64>,
}

pub(crate) fn launch_endpoint_kind(provider: &str, cloud: bool, tui: bool) -> Result<&'static str> {
    if cloud {
        if provider != "devin" {
            return Err(Error::rejected(
                "--cloud is only supported for provider devin",
            ));
        }
        if tui {
            return Err(Error::rejected("--cloud cannot be combined with --tui"));
        }
        return Ok("cloud");
    }
    if tui {
        if registry::spec_opt(provider, "pty").is_some() {
            return Ok("pty");
        }
        return Err(Error::rejected(format!(
            "provider '{provider}' has no pty endpoint — `--tui` is only \
             meaningful for claude (devin is already a TUI)"
        )));
    }
    registry::default_kind(provider)
}

/// `cadence devin [-r slug]` / `cadence codex` / `cadence claude`:
/// register the provider's endpoint, wait for it to open, then attach
/// this terminal by default where the kind has an attachable surface.
/// Re-running against an already-registered name resumes or reuses it.
/// Once open, the agent answers to its alias and its provider-native id
/// alike (e.g. `cadence agent show <devin-session-slug>`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn provider_launch(
    state_dir: &Path,
    provider: &str,
    cwd: Option<PathBuf>,
    role: &str,
    alias: Option<String>,
    resume: Option<String>,
    instructions_file: Option<PathBuf>,
    detach: bool,
    upstream: Option<String>,
    worktree: Option<&str>,
    briefing: BriefMode,
    auto_ready: bool,
    agents_md: bool,
    tui: bool,
    // `--sandbox`; `None` resolves per provider below.
    sandbox: Option<String>,
    claude: &ClaudeOpts,
    devin: &DevinOpts,
    cursor: &CursorOpts,
    codex: &CodexOpts,
    team_role: Option<&str>,
    provider_default_model: bool,
) -> Result<i32> {
    // Role instructions reach every provider but codex only through the
    // briefing — refuse before anything is registered, created or
    // launched rather than store text nothing will ever read.
    if instructions_file.is_some() && briefing == BriefMode::Off && provider != "codex" {
        return Err(Error::rejected(format!(
            "--instructions-file with --no-bootstrap is refused for provider \
             '{provider}': the instructions would have no delivery channel — \
             only codex takes them natively; every other provider receives \
             them in the briefing, which --no-bootstrap skips. Drop \
             --no-bootstrap to deliver them in the briefing"
        )));
    }
    // A codex-only setting on another provider is refused, not dropped.
    if provider != "codex" && codex.approval_policy.is_some() {
        return Err(Error::rejected("--approval-policy only applies to codex"));
    }
    // `--tui` selects the provider's pty endpoint where one exists;
    // otherwise the launch kind comes from the registry's default.
    let endpoint_kind = launch_endpoint_kind(provider, devin.cloud, tui)?;
    // `-r` on claude only makes sense on the pty endpoint — the managed
    // adapter reopens through `agent resume` and would silently drop a
    // session param it never reads. Pi has no native-session seed to
    // point `-r` at either: a managed pi worker resumes the session
    // file the adapter keeps under the state dir, via `agent resume`.
    if provider == "claude" && resume.is_some() && !tui {
        return Err(Error::rejected(
            "`--resume` on claude requires `--tui` — a managed claude agent \
             resumes with `cadence agent resume <alias>`",
        ));
    }
    if provider == "pi" && resume.is_some() {
        return Err(Error::rejected(
            "`--resume` on pi is not supported — a managed pi worker resumes \
             its stored session file with `cadence agent resume <alias>`",
        ));
    }
    // A pi worker has no permission surface to flag (CAD-544): its
    // toolset is the fixed dev allowlist, unattended — refuse the
    // claude-style knobs rather than silently drop them.
    if provider == "pi" {
        if claude.permission_mode.is_some() || claude.bypass {
            return Err(Error::rejected(
                "--permission-mode/--bypass do not apply to pi — a managed pi \
                 worker runs with its fixed tool allowlist, unattended",
            ));
        }
        if !claude.allow.is_empty() {
            return Err(Error::rejected("--allow only applies to claude"));
        }
        if claude.broker_approvals {
            return Err(Error::rejected("--broker-approvals only applies to claude"));
        }
    }
    // `-r <slug>` first resolves the slug to an already-registered agent
    // (by alias or native session id) so re-running is a reopen, not a
    // duplicate registration fighting over the same session lock.
    let mut alias = alias.clone();
    if alias.is_none() {
        if let Some(name) = &resume {
            if let Some(show) = lookup_agent(state_dir, name)? {
                let found = &show["agent"];
                let known_provider = found["provider"].as_str().unwrap_or_default();
                if known_provider != provider {
                    return Err(Error::rejected(format!(
                        "'{name}' is already registered as a {known_provider} agent \
                         (alias '{}') — use `cadence agent attach {}`",
                        found["alias"].as_str().unwrap_or_default(),
                        found["alias"].as_str().unwrap_or_default(),
                    )));
                }
                alias = found["alias"].as_str().map(str::to_string);
            }
        }
    }
    let alias = alias
        .or_else(|| resume.clone())
        .unwrap_or_else(|| format!("{provider}-{}", &Uuid::new_v4().simple().to_string()[..6]));
    // An alias keeps its provider and endpoint kind — refuse before any
    // worktree, register or resume rather than reopen the old agent under
    // the new verb or flags. The lookup fails closed: only the daemon's
    // not-found answer means "not registered" (CAD-305).
    let existing = lookup_agent(state_dir, &alias)?;
    if let Some(show) = &existing {
        if show["agent"]["alias"].as_str() == Some(alias.as_str()) {
            refuse_endpoint_mismatch(&alias, &show["agent"], provider, endpoint_kind)?;
        }
    }
    let cwd = match cwd {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    // `--worktree` creates an isolated checkout under the repo's
    // `.cadence/wt/` — refuse before creating anything when the
    // resolved agent already exists: a reopen keeps its stored cwd.
    if worktree.is_some() && existing.is_some() {
        return Err(Error::rejected(format!(
            "'{alias}' is already registered — --worktree only applies to a \
             new agent; reuse the existing checkout via --cwd"
        )));
    }
    let cwd = match worktree {
        Some(name) => create_worktree(&cwd, name)?,
        None => cwd,
    };
    if auto_ready && !registry::screen_probe(provider, endpoint_kind) {
        return Err(Error::rejected(
            "--auto-ready only applies to pty endpoints — a screen probe \
             exists only there",
        ));
    }
    let instructions = instructions_file.map(std::fs::read_to_string).transpose()?;
    let mut params_obj = serde_json::Map::new();
    if let Some(session) = &resume {
        params_obj.insert("session".to_string(), Value::String(session.clone()));
    }
    if let Some(upstream) = &upstream {
        params_obj.insert("upstream".to_string(), Value::String(upstream.clone()));
    }
    // Claude's launch params ride in `params` so the adapter replays
    // them verbatim on resume; the turn-liveness keys stay gated on the
    // endpoint spec below — managed-only, since pty liveness is the
    // pane itself, not provider event activity.
    if provider == "claude" {
        let spec = registry::spec(provider, endpoint_kind)?;
        if let Some(model) = &claude.model {
            params_obj.insert("model".to_string(), json!(model));
        }
        if let Some(effort) = &claude.effort {
            params_obj.insert("effort".to_string(), json!(effort));
        }
        let permission_mode = if claude.bypass {
            Some("bypassPermissions".to_string())
        } else {
            claude.permission_mode.clone()
        };
        if let Some(mode) = permission_mode {
            params_obj.insert("permission_mode".to_string(), json!(mode));
        }
        if !claude.allow.is_empty() {
            params_obj.insert("allowed_tools".to_string(), json!(claude.allow));
        }
        // `--broker-approvals` is managed-only: the verbs refuse it
        // with `--tui`/`--bypass` already, and the spec gate keeps any
        // non-verb path honest the same way `turn_idle_secs` is gated.
        if claude.broker_approvals {
            if tui || claude.bypass {
                return Err(Error::rejected(
                    "--broker-approvals is refused with --tui and --bypass — \
                     a pane answers its own prompts and bypass makes them moot",
                ));
            }
            if spec.launch_params.contains(&"broker_approvals") {
                params_obj.insert("broker_approvals".to_string(), json!(true));
            }
            if let Some(secs) = claude.permission_timeout_secs {
                params_obj.insert("permission_timeout_secs".to_string(), json!(secs));
            }
        }
        if spec.launch_params.contains(&"turn_idle_secs") {
            if let Some(secs) = claude.turn_idle_secs {
                params_obj.insert("turn_idle_secs".to_string(), json!(secs));
            }
        }
        if spec.launch_params.contains(&"turn_max_secs") {
            if let Some(secs) = claude.turn_max_secs {
                params_obj.insert("turn_max_secs".to_string(), json!(secs));
            }
        }
    }
    // Devin's `permission_mode` persists the same way — the profile
    // replays it into the pane argv on every open, so a `--bypass`
    // worker never stalls on its first approval menu again.
    if provider == "devin" && endpoint_kind == "pty" {
        let mode = if devin.bypass {
            Some("dangerous")
        } else {
            devin.permission_mode.as_deref()
        };
        if let Some(mode) = mode {
            registry::devin_permission_mode(mode)?;
            params_obj.insert("permission_mode".to_string(), json!(mode));
        }
    }
    if endpoint_kind == "cloud" {
        insert_devin_cloud_params(&mut params_obj, devin)?;
    }
    // Cursor's model/permission params persist the same way — the
    // profile replays them into the pane argv on every open; `--bypass`
    // stores `force`.
    if provider == "cursor" {
        if let Some(model) = &cursor.model {
            params_obj.insert("model".to_string(), json!(model));
        }
        let mode = if cursor.bypass {
            Some("force")
        } else {
            cursor.permission_mode.as_deref()
        };
        if let Some(mode) = mode {
            registry::cursor_permission_mode(mode)?;
            params_obj.insert("permission_mode".to_string(), json!(mode));
        }
    }
    if provider == "codex" {
        if let Some(model) = &codex.model {
            params_obj.insert("model".to_string(), json!(model));
        }
        if let Some(effort) = &codex.effort {
            registry::codex_effort(effort)?;
            params_obj.insert("effort".to_string(), json!(effort));
        }
        if let Some(policy) = &codex.approval_policy {
            registry::codex_approval_policy(policy)?;
            params_obj.insert("approval_policy".to_string(), json!(policy));
        }
        if let Some(secs) = codex.turn_idle_secs {
            params_obj.insert("turn_idle_secs".to_string(), json!(secs));
        }
        if let Some(secs) = codex.turn_max_secs {
            params_obj.insert("turn_max_secs".to_string(), json!(secs));
        }
    }
    // Pi's launch params ride in `params` exactly like claude's — the
    // adapter replays them verbatim on every open: `--model`, then
    // `effort` through `set_thinking_level` (verified via `get_state`).
    if provider == "pi" {
        let spec = registry::spec(provider, endpoint_kind)?;
        if let Some(model) = &claude.model {
            params_obj.insert("model".to_string(), json!(model));
        }
        if let Some(effort) = &claude.effort {
            registry::pi_effort(effort)?;
            params_obj.insert("effort".to_string(), json!(effort));
        }
        if spec.launch_params.contains(&"turn_idle_secs") {
            if let Some(secs) = claude.turn_idle_secs {
                params_obj.insert("turn_idle_secs".to_string(), json!(secs));
            }
        }
        if spec.launch_params.contains(&"turn_max_secs") {
            if let Some(secs) = claude.turn_max_secs {
                params_obj.insert("turn_max_secs".to_string(), json!(secs));
            }
        }
        // CAD-556 — the flag is tri-state: `--confine`/`--no-confine`
        // always win over the pm.yaml `[host] confine_pi_workers`
        // default the daemon applies when the key is absent entirely.
        // `--confine` refuses up front on a host without Landlock —
        // an opt-in that silently degraded to unconfined would be the
        // worst kind of surprise.
        if let Some(confine) = claude.confine {
            if confine {
                cadence_agent::confine::available()
                    .map_err(|e| Error::rejected(format!("--confine: {e}")))?;
            }
            params_obj.insert("confine".to_string(), json!(confine));
        }
    } else if claude.confine.is_some() {
        return Err(Error::rejected(
            "--confine/--no-confine only apply to provider `pi` — Landlock \
             confinement is the pi worker's today",
        ));
    }
    if auto_ready {
        params_obj.insert(
            "auto_ready".to_string(),
            Value::String("verified".to_string()),
        );
    }
    // `--agents-md` is a cadence-level opt-in, not provider config —
    // it rides in params so resume replays it like the other launch
    // params.
    if agents_md {
        params_obj.insert("agents_md".to_string(), Value::Bool(true));
    }
    if provider_default_model {
        if params_obj.contains_key("model") {
            return Err(Error::invalid(
                "conflicting_model_policy",
                "an explicit model cannot be combined with --provider-default-model",
            ));
        }
        if !registry::supports_model(provider, endpoint_kind) {
            return Err(Error::invalid(
                "unsupported_model_setting",
                format!("provider '{provider}' endpoint '{endpoint_kind}' does not accept a model"),
            ));
        }
    }
    if endpoint_kind == "cloud" {
        registry::validate_launch_params(
            provider,
            endpoint_kind,
            &Value::Object(params_obj.clone()),
        )?;
    }
    let params = (!params_obj.is_empty()).then(|| Value::Object(params_obj).to_string());
    // The sandbox rides the agent record; codex sends it on
    // `thread/start`. A cadence-launched codex worker is writable by
    // default — the same trust posture the other providers already run
    // — and `read-only` remains available when explicitly asked.
    let sandbox = sandbox.unwrap_or_else(|| {
        if provider == "codex" {
            "workspace-write"
        } else {
            "read-only"
        }
        .to_string()
    });
    // Reopening an already-registered name keeps its stored params — a
    // requested upstream is not retro-applied to a pre-existing agent.
    let mut registered_fresh = false;
    match client::rpc(
        state_dir,
        "agent_register",
        json!({"alias": alias, "provider": provider,
        "endpoint_kind": endpoint_kind, "cwd": cwd,
        "role": role, "sandbox": sandbox,
        "instructions": instructions, "params": params,
        "team_role": team_role,
        "model_policy": if provider_default_model {
            Some("provider_default")
        } else {
            None::<&str>
        }}),
    ) {
        Ok(_) => registered_fresh = true,
        Err(err) if err.to_string().contains("UNIQUE") => {
            // Already registered — reopen rather than fail. A stopped
            // agent is resumed; a live one is reused as-is.
            let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
            refuse_endpoint_mismatch(&alias, &show["agent"], provider, endpoint_kind)?;
            let state = show["agent"]["state"].as_str().unwrap_or_default();
            if matches!(state, "stopped" | "offline") {
                client::rpc(state_dir, "agent_resume", json!({"alias": alias}))?;
            }
            if upstream.is_some() {
                eprintln!(
                    "note: '{alias}' was already registered — its stored params \
                     (including upstream wiring) are unchanged"
                );
            }
        }
        Err(err) => return Err(err),
    }
    // The provider endpoint opens asynchronously (a pty open can wait on
    // the native session lock) — poll until it is live or gives up.
    // Kinds with no attachable endpoint are done once the actor is back.
    let attachable = registry::attachable(provider, endpoint_kind);
    let deadline = Instant::now() + Duration::from_secs(45);
    let (agent, unknown) = loop {
        let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
        let agent = show["agent"].clone();
        let state = agent["state"].as_str().unwrap_or_default();
        let open = agent["endpoint"].is_string();
        if open
            || matches!(state, "stopped" | "offline" | "attention")
            || (!attachable && matches!(state, "idle" | "running"))
        {
            break (agent, show["unknown"].as_i64().unwrap_or(0));
        }
        if Instant::now() >= deadline {
            break (agent, show["unknown"].as_i64().unwrap_or(0));
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let state = agent["state"].as_str().unwrap_or_default();
    // A fresh agent gets its briefing (and for joins, or an explicit
    // --bootstrap, the durable kickoff message) only once the endpoint
    // reports open — a launch that fences or stalls leaves the cwd
    // repository byte-identical. Normal gating applies on the message:
    // a pty pane still needs the ready claim.
    let opened = agent["endpoint"].is_string()
        || (!attachable && matches!(state, "idle" | "running" | "waiting_input"));
    let mut briefing_file = Value::Null;
    if opened && briefing != BriefMode::Off {
        if registered_fresh {
            let file = brief_agent(
                state_dir,
                &alias,
                briefing == BriefMode::FilesAndMessage,
                instructions.as_deref(),
            )?;
            briefing_file = json!(file);
        } else if let Ok(Some(file)) = refresh_briefing(state_dir, &alias) {
            // A re-launched pre-existing agent: regen a missing
            // briefing / re-apply an opted-in AGENTS.md — the same
            // post-open housekeeping `agent resume` runs.
            briefing_file = json!(file);
        }
    }
    let native = agent["session_id"]
        .as_str()
        .or_else(|| agent["thread_id"].as_str());
    // A fenced agent (attention, no endpoint) cannot attach — the
    // useful next step is its recovery hint, not the usual trio.
    let fenced = state == "attention" && agent["endpoint"].is_null();
    let next = if fenced {
        fenced_next(&alias, agent["error"].as_str().unwrap_or_default(), unknown)
    } else {
        json!({
            "attach": format!("cadence agent attach {alias}"),
            "ready": format!("cadence agent ready {alias}"),
            "send": format!("cadence message send {alias} --text '…'"),
        })
    };
    print_json(&json!({
        "alias": alias,
        "provider": provider,
        "state": state,
        "session": native,
        "endpoint": agent["endpoint"],
        "permission_mode": agent["params"]["permission_mode"],
        "briefing": briefing_file,
        "upstream": if registered_fresh { upstream.clone() } else { None },
        "next": next,
    }));
    if state == "starting" {
        eprintln!("still opening — watch `cadence agent show {alias}`");
    }
    // `--detach` opts out entirely; without a live endpoint there is
    // nothing to attach or print beyond the summary's `next.attach`.
    if detach || agent["endpoint"].is_null() {
        return Ok(0);
    }
    // Attach is the default — exec it only where this terminal can:
    // stdin must be a TTY and we must not sit inside tmux (a nested
    // client cannot attach a foreign socket). Otherwise print the
    // attach command exactly like `agent attach` without --run.
    if atty_stdin() && std::env::var_os("TMUX").is_none() {
        return attach_agent(state_dir, &alias, true);
    }
    attach_agent(state_dir, &alias, false)
}

/// `cadence join <group> <provider>`: resolve the group agent (alias or
/// provider-native id — `agent_show` resolves both), then launch a new
/// worker through `provider_launch` with `params.upstream` pointing at
/// the group's canonical alias. cwd defaults to the group agent's cwd.
#[allow(clippy::too_many_arguments)]
pub(crate) fn join_group(
    state_dir: &Path,
    group: &str,
    provider: &str,
    resume: Option<String>,
    tui: bool,
    detach: bool,
    cwd: Option<PathBuf>,
    alias: Option<String>,
    role: &str,
    sandbox: Option<String>,
    instructions_file: Option<PathBuf>,
    worktree: Option<String>,
    no_bootstrap: bool,
    auto_ready: bool,
    agents_md: bool,
    claude_opts: ClaudeOpts,
    devin_opts: DevinOpts,
    cursor_opts: CursorOpts,
    codex_opts: CodexOpts,
    team_role: Option<String>,
    provider_default_model: bool,
) -> Result<i32> {
    // Validate the provider before any work — the registry names the
    // supported launch verbs in the rejection.
    registry::default_kind(provider)?;
    let show = client::rpc(state_dir, "agent_show", json!({"alias": group})).map_err(|_| {
        Error::rejected(format!(
            "Unknown group '{group}' — no such agent; \
                 `cadence agent list` shows registered aliases"
        ))
    })?;
    let pm = show["agent"].clone();
    let pm_alias = pm["alias"].as_str().unwrap_or_default().to_string();
    // The resumed slug must not resolve back to the group agent —
    // an agent cannot be its own worker.
    if let Some(name) = &resume {
        if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": name})) {
            if show["agent"]["alias"].as_str() == Some(pm_alias.as_str()) {
                return Err(Error::rejected(
                    "Cannot join an agent to itself — '-r' names the group agent",
                ));
            }
        }
    }
    let cwd = cwd.or_else(|| pm["cwd"].as_str().map(PathBuf::from));
    provider_launch(
        state_dir,
        provider,
        cwd,
        role,
        alias,
        resume,
        instructions_file,
        detach,
        Some(pm_alias),
        worktree.as_deref(),
        if no_bootstrap {
            BriefMode::Off
        } else {
            BriefMode::FilesAndMessage
        },
        auto_ready,
        agents_md,
        tui,
        sandbox,
        &claude_opts,
        &devin_opts,
        &cursor_opts,
        &codex_opts,
        team_role.as_deref(),
        provider_default_model,
    )
}

/// The daemon's answer for a name that resolves to no agent, by alias or
/// provider-native id.
pub(crate) const UNKNOWN_AGENT: &str = "Unknown managed agent";

/// `agent_show` for a launch's pre-create checks, failing closed: `None`
/// only for the daemon's not-found answer. Any other error — an
/// unreachable daemon, an ambiguous native id — is returned, never read
/// as "not registered", so nothing is created on a guess (CAD-305).
pub(crate) fn lookup_agent(state_dir: &Path, name: &str) -> Result<Option<Value>> {
    found_or_absent(client::rpc(state_dir, "agent_show", json!({"alias": name})))
}

pub(crate) fn found_or_absent(shown: Result<Value>) -> Result<Option<Value>> {
    match shown {
        Ok(show) => Ok(Some(show)),
        Err(Error::Rejected(message)) if message == UNKNOWN_AGENT => Ok(None),
        Err(err) => Err(err),
    }
}

/// CAD-283 / CAD-305: reopening a registered alias under a different
/// provider, or the same provider on a different endpoint kind (managed
/// vs `--tui`, devin pty vs `--cloud`), would silently resume the old
/// agent and drop what was asked for — refuse, naming both and the
/// remove-then-launch path (there is no in-place swap).
pub(crate) fn refuse_endpoint_mismatch(
    alias: &str,
    agent: &Value,
    provider: &str,
    endpoint_kind: &str,
) -> Result<()> {
    let registered = agent["provider"].as_str().unwrap_or_default();
    if registered != provider {
        return Err(Error::rejected(format!(
            "'{alias}' is already registered as a {registered} agent, not {provider} — \
             an alias keeps its provider. To replace it, run `cadence agent remove \
             {alias}`, then launch or join {provider} under that alias"
        )));
    }
    let registered_kind = agent["endpoint_kind"].as_str().unwrap_or_default();
    if registered_kind == endpoint_kind {
        return Ok(());
    }
    Err(Error::rejected(format!(
        "'{alias}' is already registered as a {provider} {registered_kind} agent, \
         not {provider} {endpoint_kind} — an alias keeps its endpoint kind. To \
         reopen it as registered, run `cadence agent resume {alias}`; to replace \
         it, run `cadence agent remove {alias}`, then launch or join {provider} \
         {endpoint_kind} under that alias"
    )))
}
