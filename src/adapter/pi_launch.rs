//! Pi launch: environment scrubbing, command construction, paths and auth copy.

use super::*;

/// The only inherited variables a Pi MASTER keeps (CAD-322 round 2,
/// I3): its environment is cleared wholesale and only these names are
/// re-read — connectivity and locale, never credentials. Every provider
/// key (AWS_*, Moonshot/Kimi, Together, Fireworks, Baseten, Llama,
/// NVIDIA, OpenCode, Minimax, Qwen, Xiaomi, ZAI_CN, Ant Ling,
/// ANTHROPIC_AUTH_TOKEN, COPILOT_GITHUB_TOKEN, …), every `PI_*`,
/// `CLAUDE_*`, `CADENCE_*`, `GIT_*`, `XDG_*`, `LD_*`, `NODE_OPTIONS`
/// and `SSH_AUTH_SOCK` is gone however it is spelled — a denylist could
/// never enumerate the next provider's variable. The daemon context and
/// `PI_CODING_AGENT_DIR` arrive as explicit env pairs afterwards.
pub(super) const PI_MASTER_ENV_KEEP: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TERM",
    "COLORTERM",
    "SHELL",
    "TZ",
    // Connectivity config, not secrets: corporate CA roots and proxies.
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

/// The master's environment posture: cleared, then the allowlist.
/// `DENIED_ENV`'s forge tokens are re-listed only as defense in depth —
/// they are not in the keep set anyway.
pub(crate) fn pi_master_env_scrub() -> EnvScrub {
    EnvScrub::cleared_except(PI_MASTER_ENV_KEEP).and_names(crate::master::DENIED_ENV)
}

/// The only inherited variables a Pi WORKER keeps (CAD-544) — the same
/// allowlist posture as the master ([`pi_master_env_scrub`]), plus what
/// an unattended development worker needs:
/// - `TMPDIR` and the `XDG_*` dirs — tool config lands where the
///   operator put it (`XDG_CACHE_HOME` itself is overridden per agent
///   at open — CAD-570 — so the inherited value never reaches a
///   provider cache);
/// - `SSH_AUTH_SOCK`/`SSH_AGENT_PID` — git over an ssh-agent;
/// - `CARGO_HOME`, `RUSTUP_HOME`, `RUSTC_WRAPPER`, `SCCACHE_DIR` — the
///   host's Rust toolchain layout;
/// - `COLORTERM`/`TERM`/`SHELL`/`TZ`/`USER`/`LOGNAME`/`LANG`/`LC_*`,
///   `PATH`, `HOME`, and the proxy/CA roots — inherited from the master
///   keep-list unchanged.
///
/// git and gh authenticate through the operator's own files under
/// `HOME` (`~/.gitconfig`, `~/.config/gh`) — never through an inherited
/// token, so `GH_TOKEN`, `GITHUB_TOKEN` and every provider API key are
/// gone however spelled. `PI_CODING_AGENT_DIR`, `CADENCE_ALIAS`,
/// `CADENCE_STATE_DIR` and the daemon context arrive as explicit env
/// pairs afterwards.
pub(super) const PI_WORKER_ENV_KEEP_EXTRA: &[&str] = &[
    "TMPDIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTC_WRAPPER",
    "SCCACHE_DIR",
];

/// A pi worker's environment posture: cleared, then the allowlist —
/// identical mechanism to the master's, with the dev additions above.
/// `DENIED_ENV`'s forge tokens are re-listed only as defense in depth —
/// minus `SSH_AUTH_SOCK`, which the worker keep-list deliberately
/// retains (name-removal applies after the keep re-inherit, so listing
/// it would silently win over the keep entry).
pub(crate) fn pi_worker_env_scrub() -> EnvScrub {
    let keep: Vec<&str> = PI_MASTER_ENV_KEEP
        .iter()
        .chain(PI_WORKER_ENV_KEEP_EXTRA)
        .copied()
        .collect();
    let denied: Vec<&str> = crate::master::DENIED_ENV
        .iter()
        .copied()
        .filter(|name| *name != "SSH_AUTH_SOCK")
        .collect();
    EnvScrub::cleared_except(&keep)
        .and_names(&denied)
        .and_names(PI_KEY_ENV)
        .and_names(crate::adapter::CLOUD_SECRET_ENV)
}

/// Provider API-key variables a Pi WORKER must not inherit from the
/// daemon's environment — the master's child env is allowlisted
/// wholesale ([`pi_master_env_scrub`]) instead. Scrubbed by name: Pi
/// reads a fixed set, none of them `*_API_KEY`-suffixed generically.
pub(super) const PI_KEY_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "OPENAI_API_KEY_OAUTH",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "MISTRAL_API_KEY",
    "GROQ_API_KEY",
    "XAI_API_KEY",
    "OPENROUTER_API_KEY",
    "CEREBRAS_API_KEY",
    "DEEPSEEK_API_KEY",
    "ZAI_API_KEY",
    "AI_GATEWAY_API_KEY",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AZURE_OPENAI_API_KEY",
    // Round-2 review set: vendor keys Pi reads that the original list
    // missed (the master no longer depends on this list being complete).
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "MOONSHOT_API_KEY",
    "KIMI_API_KEY",
    "TOGETHER_API_KEY",
    "FIREWORKS_API_KEY",
    "BASETEN_API_KEY",
    "LLAMA_API_KEY",
    "NVIDIA_API_KEY",
    "OPENCODE_API_KEY",
    "MINIMAX_API_KEY",
    "QWEN_API_KEY",
    "DASHSCOPE_API_KEY",
    "XIAOMI_API_KEY",
    "ZAI_CN_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "COPILOT_GITHUB_TOKEN",
];

/// Provider binary; `CADENCE_PI_COMMAND` overrides it (test/mock) —
/// the mock sees the same argv shape as the real CLI.
pub(super) fn pi_command(env: &ProviderEnv) -> Vec<String> {
    if let Some(cmd) = env.var("CADENCE_PI_COMMAND") {
        let parts: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
        if !parts.is_empty() {
            return parts;
        }
    }
    vec!["pi".to_string()]
}

/// What [`resolve_pi_launch_command`] reports for the program the
/// adapter would exec: `Ok(path)` is the absolute resolved executable,
/// `Err(reason)` is the
/// clear "not found" line the daemon start log and `doctor --host`
/// both surface (CAD-1247).
pub(crate) fn resolve_pi_launch_command(
    env: &ProviderEnv,
    path: Option<&str>,
) -> std::result::Result<PathBuf, String> {
    let program = pi_command(env)
        .into_iter()
        .next()
        .unwrap_or_else(|| "pi".to_string());
    let path_display = path.unwrap_or_default();
    if !program.contains('/') {
        let path = path.ok_or_else(|| "startup resolution is indeterminate: PATH is unavailable; configure CADENCE_PI_COMMAND as an absolute executable path".to_string())?;
        for entry in path.split(':') {
            if !Path::new(entry).is_absolute() {
                return Err("startup resolution is indeterminate: PATH reaches a relative entry before a match and agents launch in their own working directories; use absolute PATH entries or CADENCE_PI_COMMAND".to_string());
            }
            if let Some(resolved) = crate::master::which(&program, Some(entry)) {
                return Ok(resolved);
            }
        }
        return Err(format!(
            "command '{program}' not found on PATH ({path_display})"
        ));
    }
    let candidate = PathBuf::from(&program);
    if !candidate.is_absolute() {
        return Err("startup resolution is indeterminate: CADENCE_PI_COMMAND is relative to each agent's working directory; configure an absolute executable path".to_string());
    }
    let executable = std::fs::metadata(&candidate).ok().is_some_and(|m| {
        use std::os::unix::fs::PermissionsExt;
        m.is_file() && m.permissions().mode() & 0o111 != 0
    });
    if executable {
        std::path::absolute(&candidate)
            .map_err(|e| format!("cannot resolve command '{program}' to an absolute path: {e}"))
    } else {
        Err(format!("command '{program}' is not executable"))
    }
}

/// The master's own Pi config dir — its `PI_CODING_AGENT_DIR`: auth
/// and extension state live here, never in the operator's `~/.pi`,
/// mirroring [`crate::master::claude_config_dir`].
pub fn pi_config_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("master").join("pi")
}

/// The generated guard extension path (`<state>/master/pi-guard.js`).
/// Plain `.js` — Pi's loader (jiti) accepts it, and Node can evaluate
/// the grammar section verbatim in tests. It must live OUTSIDE
/// `master/pi`: that dir is writable by the confined master, and a
/// writable guard could rewrite itself.
pub(super) fn pi_guard_path(state_dir: &Path) -> PathBuf {
    state_dir.join("master").join("pi-guard.js")
}

/// The runtime turn guard lives outside each agent's writable tree.
pub(super) fn pi_turn_guard_path(state_dir: &Path, alias: &str) -> Result<PathBuf> {
    let _ = pi_worker_dir(state_dir, alias)?;
    Ok(state_dir
        .join("pi-turn-guards")
        .join(alias)
        .join("guard.ts"))
}

pub(super) fn write_pi_turn_guard(state_dir: &Path, alias: &str) -> Result<PathBuf> {
    let path = pi_turn_guard_path(state_dir, alias)?;
    ensure_private_dir(path.parent().unwrap())?;
    std::fs::write(&path, include_str!("pi_turn_guard.ts"))?;
    Ok(path)
}

/// A pi WORKER's private dir under the state dir (CAD-544):
/// `<state>/agents/<alias>` holds its `PI_CODING_AGENT_DIR`
/// (`pi/`, where the copied `auth.json` lives) and its session file
/// (`session.jsonl`). Never the operator's `~/.pi`, never shared with
/// another worker. The alias is a path component, so it is checked
/// against the agent-name grammar — `[A-Za-z0-9._-]`, and never an
/// all-dots name (`.`, `..`, `...` would climb out of `agents/`).
pub(super) fn pi_worker_dir(state_dir: &Path, alias: &str) -> Result<PathBuf> {
    let ok = !alias.is_empty()
        && alias.len() <= 80
        && alias.bytes().any(|b| b != b'.')
        && alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !ok {
        return Err(Error::rejected(format!(
            "invalid worker alias '{alias}' — [A-Za-z0-9._-], 1-80 chars, not all dots"
        )));
    }
    Ok(state_dir.join("agents").join(alias))
}

/// CAD-1752: the working dir of an agent hired without a repo —
/// `<state>/agents/<alias>/home`, created 0700. Only this subdir is the
/// agent's cwd; the parent also holds its `pi/` login and session file.
pub fn pi_agent_home(state_dir: &Path, alias: &str) -> Result<PathBuf> {
    let home = pi_worker_dir(state_dir, alias)?.join("home");
    ensure_private_dir(&home)?;
    Ok(home)
}

/// The worker's private `PI_CODING_AGENT_DIR` (`<worker>/pi`) — created
/// 0700 on open, holding its copied `auth.json`.
pub(super) fn pi_worker_config_dir(state_dir: &Path, alias: &str) -> Result<PathBuf> {
    Ok(pi_worker_dir(state_dir, alias)?.join("pi"))
}

/// The worker's persistent Pi session file (`<worker>/session.jsonl`).
pub(super) fn pi_worker_session(state_dir: &Path, alias: &str) -> Result<PathBuf> {
    Ok(pi_worker_dir(state_dir, alias)?.join("session.jsonl"))
}

/// CAD-1752: where this worker's forked sessions land
/// (`<worker>/sessions`, inside the one dir its grant already covers).
pub(super) fn pi_worker_sessions_dir(state_dir: &Path, alias: &str) -> Result<PathBuf> {
    Ok(pi_worker_dir(state_dir, alias)?.join("sessions"))
}

/// The pointer to the session file this worker currently uses
/// (`<worker>/session.current`), written after every successful open
/// from Pi's own `get_state.sessionFile`. Absent = `session.jsonl`.
fn pi_session_pointer(state_dir: &Path, alias: &str) -> Result<PathBuf> {
    Ok(pi_worker_dir(state_dir, alias)?.join("session.current"))
}

/// Is `path` a real file inside `dir`? Both sides are canonicalized, so a
/// `..` segment or a symlink that resolves outside `dir` does not pass.
fn real_file_inside(dir: &Path, path: &Path) -> bool {
    match (dir.canonicalize(), path.canonicalize()) {
        (Ok(dir), Ok(path)) => path.starts_with(dir) && path.is_file(),
        _ => false,
    }
}

pub(super) fn pi_current_session(state_dir: &Path, alias: &str) -> Result<PathBuf> {
    let dir = pi_worker_dir(state_dir, alias)?;
    let pointer = pi_session_pointer(state_dir, alias)?;
    if let Ok(text) = std::fs::read_to_string(&pointer) {
        let pointed = PathBuf::from(text.trim());
        if real_file_inside(&dir, &pointed) {
            return Ok(pointed);
        }
        // CAD-1767: a pointer that leaves the worker's own dir (`..` or a
        // symlink) or names nothing is never used; say why where the
        // provider's own log lives.
        let line = format!(
            "cadence: session.current points to {} which is not a file inside {} - \
             ignored, launching from session.jsonl\n",
            pointed.display(),
            dir.display()
        );
        if let Some(parent) = dir.parent() {
            use std::io::Write;
            if let Ok(mut log) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(parent.join(format!("{alias}.provider.log")))
            {
                let _ = log.write_all(line.as_bytes());
            }
        }
        eprint!("{line}");
    }
    pi_worker_session(state_dir, alias)
}

/// Record Pi's reported session file as current, if it is inside the
/// worker's own dir. Best effort: a missed write only means the next
/// launch re-derives from the previous pointer.
pub(super) fn record_current_session(state_dir: &Path, alias: &str, reported: &str) {
    let Ok(dir) = pi_worker_dir(state_dir, alias) else {
        return;
    };
    let path = PathBuf::from(reported);
    if !real_file_inside(&dir, &path) {
        return;
    }
    let Ok(pointer) = pi_session_pointer(state_dir, alias) else {
        return;
    };
    let tmp = pointer.with_extension("tmp");
    if std::fs::write(&tmp, path.to_string_lossy().as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &pointer);
    }
}

/// The `cwd` in a Pi session file's header line.
fn session_header_cwd(path: &Path) -> Option<PathBuf> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file).read_line(&mut line).ok()?;
    let header: Value = serde_json::from_str(line.trim()).ok()?;
    header.get("cwd").and_then(Value::as_str).map(PathBuf::from)
}

/// The deterministic id of the fork that carries `source` into `workdir`:
/// the same relaunch always names the same session, so a retry after a
/// crash reuses it instead of colliding with it.
fn fork_session_id(alias: &str, workdir: &Path, source: &Path) -> String {
    let digest = crate::delegation::sha256_hex(
        format!("{alias}\0{}\0{}", workdir.display(), source.display()).as_bytes(),
    );
    format!("lane-{}", &digest[..24])
}

/// The session argv of a Pi WORKER. Pi takes a resumed session's cwd from
/// the FILE HEADER, not the process cwd, so a catalogue agent launched in
/// a different workdir than its session file records is moved with
/// `--fork <current> --session-dir <own dir> --session-id <id>`: Pi copies
/// every entry into a new file whose header carries the new workdir (the
/// old file is kept). A fork already made for the same relaunch is reopened
/// with `--session`. A fork failure ends the launch (Pi exits); there is
/// no fresh-session fallback.
pub(super) fn worker_session_args(state_dir: &Path, agent: &Agent) -> Result<Vec<String>> {
    let current = pi_current_session(state_dir, &agent.alias)?;
    // OMP requires an existing file for resume; let it mint its first file.
    // The adapter records the reported path once the first turn persists it.
    if agent.provider == "omp" && !current.exists() {
        return Ok(vec![
            "--session-dir".to_string(),
            pi_worker_dir(state_dir, &agent.alias)?
                .to_string_lossy()
                .to_string(),
        ]);
    }
    let resume = |path: &Path| {
        vec![
            if agent.provider == "omp" {
                "--resume"
            } else {
                "--session"
            }
            .to_string(),
            path.to_string_lossy().to_string(),
        ]
    };
    let is_hire = crate::agent_roles::is_catalogue_hire(agent.params.as_ref());
    let workdir = launch_workdir(agent);
    let same = |a: &Path, b: &Path| {
        a.canonicalize().unwrap_or_else(|_| a.to_path_buf())
            == b.canonicalize().unwrap_or_else(|_| b.to_path_buf())
    };
    let Some(recorded) = session_header_cwd(&current).filter(|_| is_hire) else {
        return Ok(resume(&current));
    };
    if same(&recorded, &workdir) {
        return Ok(resume(&current));
    }
    let id = fork_session_id(&agent.alias, &workdir, &current);
    let dir = pi_worker_sessions_dir(state_dir, &agent.alias)?;
    let suffix = format!("_{id}.jsonl");
    let existing = std::fs::read_dir(&dir).ok().and_then(|entries| {
        entries.flatten().map(|e| e.path()).find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(&suffix))
        })
    });
    if let Some(done) = existing {
        return Ok(resume(&done));
    }
    ensure_private_dir(&dir)?;
    Ok(vec![
        "--fork".to_string(),
        current.to_string_lossy().to_string(),
        "--session-dir".to_string(),
        dir.to_string_lossy().to_string(),
        "--session-id".to_string(),
        id,
    ])
}

/// The agent's own `XDG_CACHE_HOME` (CAD-570): the master gets
/// `<state>/master/pi/cache`, a worker `<state>/agents/<alias>/pi/
/// cache` — created 0700 on open. pi-devin's model catalog lands at
/// `$XDG_CACHE_HOME/pi-devin/models.json`; under Landlock the
/// operator's `~/.cache` is denied outright (a confined master's
/// catalog fetch EACCES'd every open on the smoke host), and
/// unconfined two agents must still never share one cache file.
pub(super) fn pi_cache_dir(state_dir: &Path, alias: &str) -> Result<PathBuf> {
    if crate::master::is_master(alias) {
        Ok(pi_config_dir(state_dir).join("cache"))
    } else {
        Ok(pi_worker_config_dir(state_dir, alias)?.join("cache"))
    }
}

/// Create `dir` (recursive) at mode 0700, tightening an existing one.
pub(super) fn ensure_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Copy the operator's `auth.json` into `dir` (0600, atomic via
/// `auth.json.tmp` rename) when the dir has no non-empty login of its
/// own — the same scoped copy `master start --copy-login` makes
/// ([`crate::master::copy_login_for`]). An existing login is never
/// overwritten: the worker's own refresh owns its file afterwards.
/// `Login::None` means the operator has no Pi login to copy — the
/// worker then fails its first turn with Pi's own "No API key" error.
pub fn copy_pi_auth(dir: &Path, operator_config: &Path) -> Result<crate::master::Login> {
    use std::os::unix::fs::OpenOptionsExt;
    ensure_private_dir(dir)?;
    let target = dir.join("auth.json");
    if std::fs::read_to_string(&target)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().map(|o| !o.is_empty()))
        == Some(true)
    {
        return Ok(crate::master::Login::Own);
    }
    let Ok(text) = std::fs::read_to_string(operator_config.join("auth.json")) else {
        return Ok(crate::master::Login::None);
    };
    if serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
        .and_then(|v| v.as_object().map(|o| !o.is_empty()))
        != Some(true)
    {
        return Ok(crate::master::Login::None);
    }
    let tmp = dir.join("auth.json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    std::io::Write::write_all(&mut file, text.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, &target)?;
    Ok(crate::master::Login::Copied)
}

/// The dev-tool allowlist a pi WORKER launches with (CAD-544): Pi's
/// full built-in set, named explicitly so the list is deliberate —
/// `--tools` also fences out any extension/custom tool that a
/// project-local config might otherwise register (with
/// `--no-extensions` there is none anyway; the flag is the belt).
pub(super) const PI_WORKER_TOOLS: &[&str] =
    &["read", "bash", "edit", "write", "grep", "find", "ls"];

/// An operator-supplied image file, never a company-configured extension.
/// The managed master only exposes these two reviewed AgenticOS GET tools;
/// all other extension tools remain absent from Pi's --tools allowlist.
pub(super) const AGENTICOS_READ_EXTENSION_ENV: &str = "CADENCE_PI_AGENTICOS_READ_EXTENSION";
pub(super) const AGENTICOS_READ_TOOLS: &[&str] = &["workspace_get", "account_get"];

pub(super) fn agenticos_read_extension(env: &ProviderEnv) -> Result<Option<PathBuf>> {
    let Some(raw) = env.var(AGENTICOS_READ_EXTENSION_ENV) else {
        return Ok(None);
    };
    let path = PathBuf::from(&raw);
    if !path.is_absolute()
        || std::fs::symlink_metadata(&path)
            .map(|meta| !meta.file_type().is_file())
            .unwrap_or(true)
        || path.canonicalize().ok().as_deref() != Some(path.as_path())
    {
        return Err(Error::rejected(format!(
            "{AGENTICOS_READ_EXTENSION_ENV} must name an existing absolute regular file with no symlink components"
        )));
    }
    Ok(Some(path))
}

/// The tracker directory used for the confinement read grant. Launch policy
/// comes from the daemon-bound settings store, not this environment path.
pub(super) fn pi_pm_dir(env: &ProviderEnv) -> Option<PathBuf> {
    env.var("CADENCE_PM_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::issue::default_dir().ok())
}

pub(super) fn settings_policy(env: &ProviderEnv) -> Result<Option<crate::pi_policy::PiPolicy>> {
    let store = env
        .settings_store()
        .ok_or_else(|| Error::rejected("Pi settings authority is not bound to this adapter"))?;
    store.pi_policy()
}

/// The operator-pinned provider packages from the transactional settings store.
/// Each `name@version` resolves under the OPERATOR's Pi npm dir
/// (`<PI_CODING_AGENT_DIR|~/.pi/agent>/npm/node_modules`) — never the
/// agent's private `PI_CODING_AGENT_DIR`, so a confined master cannot
/// pick its own extensions. Resolution is fail-closed at launch: a
/// missing package, a version drift or a malformed pin refuses the open.
pub(super) fn provider_packages(
    env: &ProviderEnv,
) -> Result<Vec<crate::pi_policy::ProviderPackage>> {
    let Some(policy) = settings_policy(env)? else {
        return Ok(vec![]);
    };
    if policy.providers.is_empty() {
        return Ok(vec![]);
    }
    let operator = crate::master::operator_provider_config(
        "pi",
        env.var("PI_CODING_AGENT_DIR"),
        env.var("HOME"),
    )
    .ok_or_else(|| {
        Error::rejected(
            "pm.yaml [pi].providers is set but the operator's Pi dir is \
             unresolvable — set PI_CODING_AGENT_DIR or HOME (CAD-559)",
        )
    })?;
    let root = operator.join("npm").join("node_modules");
    policy
        .providers
        .iter()
        .map(|spec| crate::pi_policy::resolve_package(spec, &root))
        .collect()
}

/// `pi --mode rpc …`: direct argv, no shell. The master gets
/// `--no-session` (continuity pack rebuilds context) plus
/// `--no-extensions -e <guard>` and `--tools bash` — its whole toolset
/// is the allowlisted `cadence` commands, exactly the CAD-339 posture
/// the Claude master gets from `--tools`/`dontAsk`. A WORKER (CAD-544)
/// gets `--session <file>` (its persistent conversation), the dev
/// toolset [`PI_WORKER_TOOLS`], no extension guard, and reads repo
/// context files like any other worker. `--offline`/`--no-*` keep
/// startup deterministic: no update checks, skills, prompt-templates
/// or themes.
pub(super) fn build_command(
    env: &ProviderEnv,
    agent: &Agent,
    state_dir: &Path,
) -> Result<Vec<String>> {
    let params = agent.params.clone().unwrap_or(Value::Null);
    let mut cmd = pi_command(env);
    cmd.extend(["--mode".to_string(), "rpc".to_string()]);
    if crate::master::is_master(&agent.alias) {
        cmd.push("--no-session".to_string());
    } else {
        cmd.extend(worker_session_args(state_dir, agent)?);
    }
    for flag in [
        "--offline",
        "--no-themes",
        "--no-skills",
        "--no-prompt-templates",
    ] {
        cmd.push(flag.to_string());
    }
    if crate::master::is_master(&agent.alias) {
        cmd.push("--no-context-files".to_string());
    }
    // CAD-559: the only extensions loaded are the generated guard
    // (master) plus the operator's pinned `[pi].providers` packages —
    // `--no-extensions` still fences off everything else, so each entry
    // is an explicit `-e` file inside its pinned package dir.
    let packages = provider_packages(env)?;
    // Admission must close before another extension's awaited turn_end
    // listener can yield. Load the turn guard first for both roles.
    cmd.extend([
        "--no-extensions".to_string(),
        "--extension".to_string(),
        pi_turn_guard_path(state_dir, &agent.alias)?
            .to_string_lossy()
            .to_string(),
    ]);
    if crate::master::is_master(&agent.alias) {
        let agenticos_read = agenticos_read_extension(env)?;
        cmd.extend([
            "--extension".to_string(),
            pi_guard_path(state_dir).to_string_lossy().to_string(),
        ]);
        for pkg in &packages {
            for entry in &pkg.entries {
                cmd.extend([
                    "--extension".to_string(),
                    entry.to_string_lossy().to_string(),
                ]);
            }
        }
        if let Some(path) = &agenticos_read {
            cmd.extend([
                "--extension".to_string(),
                path.to_string_lossy().to_string(),
            ]);
        }
        // bash (guard-checked `cadence` verbs), `read` and `write`.
        // The guard confines both file tools to `master/tmp`: read
        // re-opens Pi's spilled bash output (CAD-552); write is how
        // the master stages `--file` input (CAD-614).
        let mut tools = vec!["bash", "read", "write"];
        if agenticos_read.is_some() {
            tools.extend(AGENTICOS_READ_TOOLS);
        }
        cmd.extend(["--tools".to_string(), tools.join(",")]);
    } else {
        for pkg in &packages {
            for entry in &pkg.entries {
                cmd.extend([
                    "--extension".to_string(),
                    entry.to_string_lossy().to_string(),
                ]);
            }
        }
        cmd.extend(["--tools".to_string(), PI_WORKER_TOOLS.join(",")]);
    }
    // `agent.model` is the provider's report, never a launch param —
    // only the configured param is replayed (same rule as Claude).
    if let Some(model) = params.get("model").and_then(Value::as_str) {
        cmd.extend(["--model".to_string(), model.to_string()]);
    }
    Ok(cmd)
}

/// The full launch argv: [`build_command`], and for a confined
/// endpoint that line wrapped in `cadence confine` — the master
/// always (CAD-439), a worker when its `confine` param asks
/// (CAD-556). A confined launch on a host without Landlock is
/// refused, never silently unconfined.
pub(super) fn launch_command(
    env: &ProviderEnv,
    state_dir: &Path,
    agent: &Agent,
) -> Result<(Vec<String>, Option<crate::confine::Policy>)> {
    let command = build_command(env, agent, state_dir)?;
    if master_confined(env, agent) {
        let (confine, policy) = pi_master_confinement(env, state_dir);
        let policy = crate::home::validate_policy_for_env(policy, env)?;
        let argv = crate::master::confine_argv(&confine, &policy, &command);
        return Ok((argv, Some(policy)));
    }
    if crate::master::is_master(&agent.alias) && env.workspace().is_some() && !env.company_master()
    {
        return Err(Error::rejected(
            "cannot isolate operational upload workspace without effective Master confinement",
        ));
    }
    if !worker_confined(agent) {
        return Ok((command, None));
    }
    // Opt-in confinement cannot quietly lapse: `--confine` on a host
    // that cannot confine is an error, not a plain launch.
    crate::master::confinement_available(env)?;
    // CAD-1380: a member-bound personal assistant (`params.assist`) gets
    // the member-boundary policy — its own principal-owned subdir and
    // worker dir only, never the tracker (`pm_dir`), the worktree/git
    // targets, the toolchain caches or `$HOME` the generic worker grant
    // carries. A member assistant is read-mostly; a build/fleet write
    // set would let one member's chat read another lane's store.
    let params = agent.params.clone().unwrap_or(Value::Null);
    let (confine, policy) = if params.get("assist").is_some() {
        pi_assistant_confinement(env, state_dir, agent)
    } else {
        pi_worker_confinement(env, state_dir, agent)
    };
    let argv = crate::master::confine_argv(&confine, &policy, &command);
    Ok((argv, Some(policy)))
}

#[cfg(test)]
mod cad1752_session_argv_tests {
    use super::*;
    use serde_json::json;

    fn hire(state: &Path, workdir: Option<&Path>) -> Agent {
        let mut params = json!({"catalogue": "developer", "confine": true});
        if let Some(dir) = workdir {
            params["launch_workdir"] = json!(dir.to_string_lossy());
        }
        Agent {
            alias: "dev-2".into(),
            uid: "agt-t".into(),
            provider: "pi".into(),
            endpoint_kind: "managed".into(),
            role: "worker".into(),
            team_role: None,
            cwd: state
                .join("agents/dev-2/home")
                .to_string_lossy()
                .into_owned(),
            sandbox: "read-only".into(),
            instructions: None,
            thread_id: None,
            session_id: None,
            model: None,
            effort: None,
            pid: None,
            pid_start: None,
            endpoint: None,
            params: Some(params),
            model_selection: None,
            quota: None,
            generation: None,
            state: "idle".into(),
            enabled: true,
            gc_keep: false,
            archived_at: None,
            archived_by: None,
            archive_reason: None,
            lifecycle_rev: 0,
            error: None,
            created: 0.0,
            updated: 0.0,
        }
    }

    #[test]
    fn lane_and_home_relaunches_fork_and_a_matching_cwd_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let worker = state.join("agents/dev-2");
        let home = worker.join("home");
        let lane = tmp.path().join("repo/.cadence/wt/t-1");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&lane).unwrap();
        let session = worker.join("session.jsonl");
        std::fs::write(
            &session,
            format!(
                "{}\n",
                json!({"type": "session", "cwd": home.to_string_lossy()})
            ),
        )
        .unwrap();
        let sessions = worker.join("sessions");

        // Home session, lane workdir: fork into the agent's own sessions dir.
        let argv = worker_session_args(state, &hire(state, Some(&lane))).unwrap();
        let id = fork_session_id("dev-2", &lane, &session);
        assert_eq!(
            argv,
            vec![
                "--fork".to_string(),
                session.to_string_lossy().to_string(),
                "--session-dir".to_string(),
                sessions.to_string_lossy().to_string(),
                "--session-id".to_string(),
                id.clone(),
            ]
        );

        // Same cwd as the header: plain resume.
        let argv = worker_session_args(state, &hire(state, None)).unwrap();
        assert_eq!(
            argv,
            vec![
                "--session".to_string(),
                session.to_string_lossy().to_string()
            ]
        );

        // The fork exists and is current; back home forks again from it.
        let forked = sessions.join(format!("2026-01-01T00-00-00-000Z_{id}.jsonl"));
        std::fs::write(
            &forked,
            format!(
                "{}\n",
                json!({"type": "session", "cwd": lane.to_string_lossy()})
            ),
        )
        .unwrap();
        record_current_session(state, "dev-2", &forked.to_string_lossy());
        let argv = worker_session_args(state, &hire(state, None)).unwrap();
        assert_eq!(argv[0], "--fork");
        assert_eq!(argv[1], forked.to_string_lossy());
        assert_eq!(argv[5], fork_session_id("dev-2", &home, &forked));
        // Still in the lane: resume the fork.
        let argv = worker_session_args(state, &hire(state, Some(&lane))).unwrap();
        assert_eq!(
            argv,
            vec![
                "--session".to_string(),
                forked.to_string_lossy().to_string()
            ]
        );
    }
}
