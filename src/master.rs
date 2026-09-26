//! The master agent (CAD-339): one per install, alias `master`, a
//! managed Claude or Pi session (CAD-322; Codex waits for a read-only
//! sandbox) the daemon starts from the agent files `agents/master/
//! SOUL.md` and `AGENT.md`.
//!
//! **Where the files live.** CAD-338 (the agent filesystem) is not
//! implemented yet, so this uses the smallest location its design
//! record names: `<pm>/agents/<slug>/` — the tracker dir (`~/pm` or
//! `CADENCE_PM_DIR`), which is already git and already has one writer.
//! `agents/` holds no `project.yaml`, so it is never read as a project.
//! The repo carries the default templates (`agents/master/`); the
//! daemon installs any that are missing on `master start`.
//!
//! **One writer.** SOUL.md and AGENT.md change only through
//! [`write_file`] (`cadence master edit`, daemon RPC `agent_file_write`),
//! which the daemon runs for the proven operator only — an agent, the
//! master included, is refused before anything is written. Each write
//! (and each install) records the files' digest in the state dir;
//! `master start` refuses files whose digest does not match — an edit
//! made around the writer is caught at the next launch instead of
//! becoming the master's instructions.
//!
//! **Launch hardening.** The master never implements, so its Claude
//! session runs in an empty cwd under the state dir, with no settings
//! files, hooks or MCP servers (`--restricted`, `--strict-mcp-config`),
//! only the Bash tool, `dontAsk`, and exactly the `cadence` subcommands
//! in [`CLAUDE_ALLOWED_TOOLS`]; forge and platform credentials are
//! dropped from its env ([`DENIED_ENV`], an empty `GH_CONFIG_DIR`). All
//! of it is keyed on the alias, not on stored params. The daemon's own
//! allowlist (`daemon::MASTER_ALLOWED`) is the second line.
//!
//! **Read confinement (CAD-439).** The allowlist does not bound what the
//! master reads: Claude Code auto-allows the Bash commands it deems
//! read-only (`id`, `ps`, `echo <glob>`, `cat` inside its working dirs)
//! in every permission mode, `dontAsk` included, and an allowlisted
//! `cadence report file`/`master escalate --file <path>` reads any path.
//! So the daemon launches the provider under `cadence confine`
//! ([`crate::confine`], Linux Landlock) with [`confinement`]: the
//! system trees, the Claude CLI's own state, the programs it runs, the
//! tracker, and the master's own dirs — nothing else of `$HOME`, the
//! state dir or `/proc`. Every descendant inherits it, detached or not.
//! Chosen after, in order: (a) no Claude Code flag, setting or mode
//! turns read-only auto-allow off — the docs say the set "is not
//! configurable", and `--restricted` confines only the file tools;
//! (b) a `--disallowedTools` list cannot be complete for the same
//! reason (the set is undocumented and version-dependent, and deny rules
//! miss `/usr/bin/cat`-style forms); (c) bubblewrap, including Claude
//! Code's own Bash sandbox, needs unprivileged user namespaces, which
//! Ubuntu 24.04+ refuses by default — Landlock needs none.
//!
//! The confined CLI has its own config dir ([`claude_config_dir`], its
//! `CLAUDE_CONFIG_DIR`): nothing of the operator's `~/.claude` or
//! `~/.claude.json` is in the policy — a confined CLI could otherwise
//! only rewrite that shared file in place, unlocked, and a writable
//! `~/.claude/settings.json` would plant hooks in every operator
//! session. By default (operator decision, 2026-09-24) the master has
//! its own, separate login: `master start` copies nothing, reports
//! `login: none` and prints [`login_command`]
//! (`CLAUDE_CONFIG_DIR=<state>/master/claude claude auth login`), and
//! Needs-you shows that command while the dir holds no login. `master
//! start --copy-login` is the opt-in copy ([`copy_login`]), with its
//! token-rotation risk. Residue: the master's tree can read its own
//! login.
//!
//! Where Landlock is unavailable (macOS, older kernels) `master start`
//! refuses; `master start --unconfined` (operator decision) starts it
//! unwrapped with its own warning, a `master_started_unconfined` event
//! and a Needs-you info row while it runs. A macOS `sandbox-exec`
//! profile is a separate ticket.
//!
//! The tracker is in the write set (`cadence report file` commits into
//! it), so its `.git/hooks` and `.git/config` are too: Landlock grants
//! whole subtrees and cannot carve those out. Defence in depth only — no
//! allowlisted verb writes a caller-chosen path there.
//!
//! Beyond the master's own tree this is a process guard, not a security
//! boundary: another same-uid process can still read credential files
//! and edit the tracker by hand (see docs/design/AGENT-FILESYSTEM.md).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::issue::Pm;

/// The master's alias — and its agent slug.
pub const ALIAS: &str = "master";

/// The providers `master start` accepts — `claude`, and `pi` since
/// CAD-322 (managed `pi --mode rpc`, same Landlock confinement and
/// cadence-command-only posture). A Codex master waits for a read-only
/// sandbox with its writes through daemon verbs. The setup wizard
/// (CAD-448) offers exactly this list with each one's exact start
/// command, so the daemon's refusal and the wizard's offer stay in one
/// place.
pub const PROVIDERS: &[&str] = &["claude", "pi"];
/// The agent files the briefing is built from, in briefing order.
pub const FILES: [&str; 2] = ["SOUL.md", "AGENT.md"];
/// Size caps from the agent-filesystem design record (characters).
pub const SOUL_MAX_CHARS: usize = 4_000;
pub const AGENT_MAX_CHARS: usize = 20_000;

const SOUL_TEMPLATE: &str = include_str!("../agents/master/SOUL.md");
const AGENT_TEMPLATE: &str = include_str!("../agents/master/AGENT.md");

/// The exact `cadence` subcommands the master's Claude session may run
/// — its whole toolset. Nothing else is allowed: the session runs in
/// `dontAsk` mode, so anything not listed is denied without a prompt.
/// Never a bare `Bash(cadence *)`: `cadence build-slot run -- <argv>`
/// execs anything (review round 1, C1).
pub const CLAUDE_ALLOWED_TOOLS: &[&str] = &[
    "Bash(cadence issue ls)",
    "Bash(cadence issue ls *)",
    "Bash(cadence issue show *)",
    "Bash(cadence issue project ls)",
    "Bash(cadence issue project ls *)",
    "Bash(cadence plan show *)",
    "Bash(cadence plan propose *)",
    "Bash(cadence project new *)",
    "Bash(cadence master dispatch *)",
    "Bash(cadence master escalate *)",
    "Bash(cadence master summary)",
    "Bash(cadence master summary *)",
    "Bash(cadence interrupt *)",
    "Bash(cadence report file *)",
    "Bash(cadence agent list)",
    "Bash(cadence agent list *)",
    "Bash(cadence agent show *)",
    "Bash(cadence status)",
    "Bash(cadence status *)",
];

/// The only built-in tool the master's Claude session has (`--tools`):
/// Bash, narrowed by [`CLAUDE_ALLOWED_TOOLS`]. No Read/Edit/Write/Web.
pub const CLAUDE_TOOLS: &str = "Bash";

/// Tools the master's Claude session may never use, whatever its
/// stored params say — belt and braces over `--tools`/`dontAsk`.
pub const CLAUDE_DISALLOWED_TOOLS: &[&str] = &[
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "Bash(gh)",
    "Bash(gh *)",
    "Bash(git push *)",
    "Bash(git merge *)",
    "Bash(git commit *)",
];

/// Forge and platform credentials removed from the master's env. The
/// scrub is by name; a credential stored in a file (gh's `hosts.yml`,
/// ssh keys) is outside the master's [`confinement`] (CAD-439).
pub const DENIED_ENV: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GITLAB_TOKEN",
    "GL_TOKEN",
    "SSH_AUTH_SOCK",
    "CLOUDFLARE_API_TOKEN",
    "CLOUDFLARE_API_KEY",
    "CF_API_TOKEN",
    "VERCEL_TOKEN",
    "NETLIFY_AUTH_TOKEN",
    "FLY_API_TOKEN",
    "NPM_TOKEN",
    "CARGO_REGISTRY_TOKEN",
];

/// The source every master wake (CAD-445) is queued under.
pub const WAKE_SOURCE: &str = "wake";

/// A master wake's `(kind, dedupe key)` for `event` about `key`
/// (`blocker_done`, `D-3/D-2`) — see `daemon/master_wake.rs`.
pub fn wake_key(event: &str, key: &str) -> (&'static str, String) {
    (WAKE_SOURCE, format!("{event}/{key}"))
}

/// The message id the master's wake for `(event, key)` is queued under.
pub fn wake_id(event: &str, key: &str) -> String {
    let (kind, key) = wake_key(event, key);
    crate::proto::daemon_message_id(kind, &key)
}

pub fn is_master(alias: &str) -> bool {
    alias == ALIAS
}

/// Env the master's provider gets on top of the usual identity pair:
/// `gh` finds no stored login (an empty config dir under the state dir),
/// git never prompts for credentials, and the tracker is named
/// explicitly — the master's cwd is not the tracker.
///
/// Confined (CAD-439), the CLI also gets its own config dir
/// ([`provider_config_dir`] — `master/claude` or `master/pi`); an
/// unconfined master (`master start --unconfined`, a host without
/// Landlock) keeps the operator's.
pub fn env_overrides(
    provider: &str,
    state_dir: &Path,
    pm_dir: Option<&Path>,
    confined: bool,
) -> Vec<(String, String)> {
    let gh = state_dir.join("master").join("no-forge");
    let _ = std::fs::create_dir_all(&gh);
    let tmp = tmpdir(state_dir);
    let _ = std::fs::create_dir_all(&tmp);
    let mut env = vec![
        (
            "GH_CONFIG_DIR".to_string(),
            gh.to_string_lossy().to_string(),
        ),
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        // CAD-439: `/tmp` is outside its confinement.
        ("TMPDIR".to_string(), tmp.to_string_lossy().to_string()),
    ];
    if confined {
        env.push((
            provider_config_env(provider).to_string(),
            provider_config_dir(provider, state_dir)
                .to_string_lossy()
                .to_string(),
        ));
    }
    if let Some(pm) = pm_dir {
        env.push((
            "CADENCE_PM_DIR".to_string(),
            pm.to_string_lossy().to_string(),
        ));
    }
    env
}

/// System trees the master's process tree may read and execute:
/// binaries, shared libraries, `/etc` (TLS roots, resolver, passwd) and
/// `/sys`. `/proc` is not listed whole — other processes' command lines
/// stay unreadable; only the provider's own `/proc/self` (resolved at
/// launch, so it is the provider's pid) and a few host-wide files.
pub const CONFINE_SYSTEM_READ: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/etc",
    "/sys",
    "/proc/self",
    "/proc/version",
    "/proc/filesystems",
    "/proc/meminfo",
    "/proc/cpuinfo",
    "/proc/stat",
    "/proc/loadavg",
    "/proc/uptime",
    "/proc/sys/vm/overcommit_memory",
    "/proc/sys/vm/mmap_min_addr",
    "/proc/sys/kernel/pid_max",
];

/// Device files the master's process tree may use — not `/dev` whole
/// (`/dev/shm` holds other processes' shared memory), and no pty: the
/// managed master talks over pipes.
pub const CONFINE_SYSTEM_WRITE: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
];

/// The native Claude CLI's install tree under `$HOME` — the Claude
/// master's `home_read`; Pi names nothing here (its npm package arrives
/// via `programs`), so a Pi master never sees it (CAD-322 round 2, N1).
pub const CONFINE_HOME_READ: &[&str] = &[".local/share/claude"];

/// Daemon env naming extra paths (`:`-separated) the master may read,
/// or also write — for a provider installed somewhere the defaults do
/// not cover. Set by whoever starts the daemon, never by an agent.
pub const CONFINE_EXTRA_READ_ENV: &str = "CADENCE_MASTER_CONFINE_READ";
pub const CONFINE_EXTRA_WRITE_ENV: &str = "CADENCE_MASTER_CONFINE_WRITE";

/// What the master's confinement is computed from. The provider-named
/// sets keep one provider's master out of another's dirs (CAD-322
/// round 2, N1): a Pi master gets `master/pi` — never R+W on
/// `master/claude` — and vice versa.
pub struct ConfineInputs {
    pub state_dir: PathBuf,
    pub home: Option<PathBuf>,
    pub pm_dir: Option<PathBuf>,
    /// Programs the master's process tree runs — the provider CLI (and
    /// its `#!` interpreter), `cadence`. Each one's real directory is
    /// readable.
    pub programs: Vec<PathBuf>,
    /// The provider's private dir under the state dir, writable
    /// (`master/claude`, `master/pi`) — only its own.
    pub provider_dir: PathBuf,
    /// Provider install trees under `$HOME` (relative names) the master
    /// may read — `claude` names [`CONFINE_HOME_READ`], `pi` none.
    pub home_read: &'static [&'static str],
    pub extra_read: Vec<PathBuf>,
    pub extra_write: Vec<PathBuf>,
}

/// CAD-439: the filesystem the master's process tree — the provider CLI
/// and every command it runs — may touch. Claude Code auto-allows the
/// read-only Bash commands it recognises (`id`, `ps`, `echo <glob>`,
/// `cat` inside its working dirs …) in every permission mode, `dontAsk`
/// included, and the set is not configurable; an allowlisted
/// `cadence report file --file <path>` reads any path it is given. So
/// the boundary is the OS: [`crate::confine`] (Landlock) exposes only
/// the system trees, the master's own dirs under the state dir (cwd,
/// tmp, briefing), the tracker (`cadence issue`/`report` read and commit
/// it directly), the provider CLI's own state (`provider_dir`, and
/// `home_read` under `$HOME`), and the programs it runs. `$HOME` —
/// ssh keys, forge logins, other repos — and the daemon's store stay
/// unreadable. The daemon socket is reached by connect, which the
/// sandbox does not restrict.
pub fn confinement(inputs: &ConfineInputs) -> crate::confine::Policy {
    let mut read: Vec<PathBuf> = CONFINE_SYSTEM_READ.iter().map(PathBuf::from).collect();
    let mut write: Vec<PathBuf> = CONFINE_SYSTEM_WRITE.iter().map(PathBuf::from).collect();
    if let Some(home) = &inputs.home {
        read.extend(inputs.home_read.iter().map(|p| home.join(p)));
    }
    for program in &inputs.programs {
        for dir in program_dirs(program) {
            if !read.contains(&dir) {
                read.push(dir);
            }
        }
    }
    read.push(crate::client::briefing_path(
        &inputs.state_dir,
        &Value::Null,
        ALIAS,
    ));
    // CAD-524: a state dir inside a sandbox gates every `cadence` verb
    // the master runs — `sandbox::adopt` → `owner_of` must READ the
    // root's marker, and Landlock permits lstat without a grant but not
    // the file read. Grant the marker FILE, never the root. Production
    // state dirs aren't `<root>/state` beside a marker — nothing added.
    if let Some(marker) = crate::sandbox::marker_for(&inputs.state_dir) {
        read.push(marker);
    }
    write.push(workdir(&inputs.state_dir));
    write.push(tmpdir(&inputs.state_dir));
    write.push(inputs.provider_dir.clone());
    if let Some(pm) = &inputs.pm_dir {
        write.push(pm.clone());
    }
    read.extend(inputs.extra_read.iter().cloned());
    write.extend(inputs.extra_write.iter().cloned());
    crate::confine::Policy { read, write }
}

/// The directory holding `program`'s real file (symlinks resolved), so
/// the whole install — a native build's versions dir, an npm package —
/// is readable; for a `cadence` release (`<releases>/<sha>/cadence`,
/// `<releases>/v<version>/cadence`) the whole releases root, so a
/// `cadence upgrade` repointing the link mid-run leaves the master's
/// verbs runnable (it holds only release binaries). Nothing when the
/// program does not exist.
fn program_dirs(program: &Path) -> Vec<PathBuf> {
    let Ok(real) = program.canonicalize() else {
        return vec![];
    };
    if let Some(root) = crate::upgrade::releases_dir_of(&real) {
        return vec![root];
    }
    real.parent().map(Path::to_path_buf).into_iter().collect()
}

/// `name` resolved against `path` (a `PATH` value) the way `execvp`
/// does; an absolute or relative path with a `/` is taken as is.
pub fn which(name: &str, path: Option<&str>) -> Option<PathBuf> {
    if name.contains('/') {
        return Some(PathBuf::from(name));
    }
    use std::os::unix::fs::PermissionsExt;
    path?.split(':').filter(|d| !d.is_empty()).find_map(|dir| {
        let candidate = Path::new(dir).join(name);
        let meta = std::fs::metadata(&candidate).ok()?;
        (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(candidate)
    })
}

/// `program` plus the interpreter its `#!` line names (`/usr/bin/env
/// node` resolves `node` on `path`) — an npm-installed Claude CLI is a
/// node script.
pub fn with_interpreter(program: &Path, path: Option<&str>) -> Vec<PathBuf> {
    let mut out = vec![program.to_path_buf()];
    let mut head = [0u8; 256];
    let n = std::fs::File::open(program)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
        .unwrap_or(0);
    let head = String::from_utf8_lossy(&head[..n]);
    if let Some(line) = head.strip_prefix("#!").and_then(|h| h.lines().next()) {
        let mut words = line.split_whitespace();
        if let Some(interp) = words.next() {
            if interp.ends_with("/env") {
                if let Some(found) = words
                    .find(|w| !w.starts_with('-'))
                    .and_then(|w| which(w, path))
                {
                    out.push(found);
                }
            } else {
                out.push(PathBuf::from(interp));
            }
        }
    }
    out
}

/// Split a `:`-separated env value into paths.
pub fn split_paths(value: Option<String>) -> Vec<PathBuf> {
    value
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// `cadence confine <policy> -- <command>`: the master's provider
/// command wrapped in its confinement. `confine_exe` is the cadence
/// binary that applies it.
pub fn confine_argv(
    confine_exe: &str,
    policy: &crate::confine::Policy,
    command: &[String],
) -> Vec<String> {
    let mut argv = vec![confine_exe.to_string(), "confine".to_string()];
    argv.extend(policy.to_args());
    argv.push("--".to_string());
    argv.extend(command.iter().cloned());
    argv
}

/// The master's own Claude config dir (its `CLAUDE_CONFIG_DIR`, CAD-439
/// review): the CLI's state — session transcripts for `--resume`, its
/// `.claude.json`, shell snapshots, its login — lives here, never in the
/// operator's shared `~/.claude` / `~/.claude.json`, which a confined
/// CLI could only rewrite in place, unlocked. Nothing of the operator's
/// config is in the master's confinement.
pub fn claude_config_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("master").join("claude")
}

/// Is the master launched unconfined — `master start --unconfined` on a
/// host without Landlock (operator decision, CAD-439 review)? Only the
/// operator-only `master_start` writes this param; `agent_set` cannot.
pub fn unconfined(params: Option<&Value>) -> bool {
    params
        .and_then(|p| p.get("unconfined"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Debug-build test seam: this daemon's own `ProviderEnv` (never the
/// process env) simulating a host without Landlock.
pub const TEST_NO_LANDLOCK: &str = "CADENCE_TEST_NO_LANDLOCK";

/// Ok when this daemon can confine the master.
pub fn confinement_available(env: &crate::adapter::ProviderEnv) -> Result<()> {
    #[cfg(debug_assertions)]
    if env.own(TEST_NO_LANDLOCK).is_some() {
        return Err(Error::provider(
            "cadence confine: Landlock is unavailable (test seam)",
        ));
    }
    let _ = env;
    crate::confine::available()
}

/// What the operator opts into when `master start --unconfined` is the
/// only way — `master start` answers with it, and the setup wizard
/// shows it next to the command it offers (CAD-448 review, I1).
pub const UNCONFINED_WARNING: &str = "the master runs UNCONFINED: no filesystem sandbox on this \
     host, so it can read and write your files (ssh keys, forge logins, every repo) — its Bash \
     allowlist is the only limit";

/// The master's Claude login, as `master start` found or made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Login {
    /// The master's config dir has a login — its own `claude auth
    /// login`, or an earlier `--copy-login` — left alone.
    Own,
    /// Copied from the operator's Claude config (`--copy-login`).
    Copied,
    /// None: the master's CLI cannot authenticate until the operator
    /// runs [`login_command`].
    None,
}

impl Login {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Copied => "copied",
            Self::None => "none",
        }
    }
}

/// The command that gives the master its own Claude login (operator
/// decision, 2026-09-24: a separate login is the default).
pub fn login_command(state_dir: &Path) -> String {
    format!(
        "CLAUDE_CONFIG_DIR={} claude auth login",
        claude_config_dir(state_dir).display()
    )
}

/// Does the master's config dir hold a login?
pub fn has_login(state_dir: &Path) -> bool {
    claude_config_dir(state_dir)
        .join(".credentials.json")
        .is_file()
}

/// The env var naming `provider`'s config dir (`CLAUDE_CONFIG_DIR`,
/// `PI_CODING_AGENT_DIR`) — used to find the operator's own login for
/// `--copy-login`.
pub fn provider_config_env(provider: &str) -> &'static str {
    match provider {
        "pi" => "PI_CODING_AGENT_DIR",
        _ => "CLAUDE_CONFIG_DIR",
    }
}

/// The provider's private config dir under the state dir — where its
/// login lives (Claude: `master/claude`; Pi: `master/pi`, CAD-322).
pub fn provider_config_dir(provider: &str, state_dir: &Path) -> PathBuf {
    match provider {
        "pi" => crate::adapter::pi::pi_config_dir(state_dir),
        _ => claude_config_dir(state_dir),
    }
}

/// Does the master's config dir hold a login for `provider`? For Pi
/// the file must also be a non-empty object: an `auth.json` of `{}`
/// (created by a bare `pi` run) proves nothing (CAD-322). Only shape
/// is checked — contents are credentials, never read into logs.
pub fn has_login_for(provider: &str, state_dir: &Path) -> bool {
    match provider {
        "pi" => std::fs::read_to_string(provider_config_dir("pi", state_dir).join("auth.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_object().map(|o| !o.is_empty()))
            .unwrap_or(false),
        _ => has_login(state_dir),
    }
}

/// The command that gives the master its own login for `provider`.
/// Pi has no non-interactive login verb — the operator runs the TUI
/// against the master's config dir and types `/login`.
pub fn login_command_for(provider: &str, state_dir: &Path) -> String {
    match provider {
        "pi" => format!(
            "PI_CODING_AGENT_DIR={} pi  # then type /login",
            provider_config_dir("pi", state_dir).display()
        ),
        _ => login_command(state_dir),
    }
}

/// Create the master's provider config dir (0700); report whether it
/// holds a login for `provider`. Copies nothing.
pub fn ensure_config_dir_for(provider: &str, state_dir: &Path) -> Result<Login> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = provider_config_dir(provider, state_dir);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(if has_login_for(provider, state_dir) {
        Login::Own
    } else {
        Login::None
    })
}

/// `master start --copy-login` for `provider`: Claude copies the
/// `claudeAiOauth` entry of `.credentials.json`; Pi copies the whole
/// `auth.json` (its only credential file). Never via env or argv.
pub fn copy_login_for(provider: &str, state_dir: &Path, operator_config: &Path) -> Result<Login> {
    if provider != "pi" {
        return copy_login(state_dir, operator_config);
    }
    crate::adapter::pi::copy_pi_auth(&provider_config_dir("pi", state_dir), operator_config)
}

/// The operator's own config dir for `provider` (for `--copy-login`):
/// Claude honours `CLAUDE_CONFIG_DIR`, Pi honours
/// `PI_CODING_AGENT_DIR` (default `~/.pi/agent`).
pub fn operator_provider_config(
    provider: &str,
    config_dir: Option<String>,
    home: Option<String>,
) -> Option<PathBuf> {
    match provider {
        "pi" => config_dir
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                home.filter(|h| !h.is_empty())
                    .map(|h| Path::new(&h).join(".pi").join("agent"))
            }),
        _ => operator_claude_config(config_dir, home),
    }
}

/// Create the master's config dir, or tighten an existing one, to 0700;
/// report whether it holds a login. Copies nothing.
pub fn ensure_config_dir(state_dir: &Path) -> Result<Login> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = claude_config_dir(state_dir);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    // `mode` applies only to a dir this call creates.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(if has_login(state_dir) {
        Login::Own
    } else {
        Login::None
    })
}

/// `master start --copy-login` (the operator's explicit opt-in): give a
/// master with no login a copy of the operator's — the `claudeAiOauth`
/// entry of its `.credentials.json` only (never its MCP server tokens),
/// mode 0600 in a 0700 dir, never via env or argv. An existing login is
/// never overwritten.
///
/// Token rotation: both copies hold the same refresh token. Each side
/// refreshes its access token into its own file; if the provider
/// rotates refresh tokens, a refresh on one side can invalidate the
/// other, and that side must sign in again. That is why the default is
/// a separate login ([`login_command`]).
pub fn copy_login(state_dir: &Path, operator_config: &Path) -> Result<Login> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    if ensure_config_dir(state_dir)? == Login::Own {
        return Ok(Login::Own);
    }
    let dir = claude_config_dir(state_dir);
    let target = dir.join(".credentials.json");
    let Ok(text) = std::fs::read_to_string(operator_config.join(".credentials.json")) else {
        return Ok(Login::None);
    };
    let oauth = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("claudeAiOauth").cloned())
        .filter(Value::is_object);
    let Some(oauth) = oauth else {
        return Ok(Login::None);
    };
    let tmp = dir.join(".credentials.json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(json!({"claudeAiOauth": oauth}).to_string().as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, &target)?;
    Ok(Login::Copied)
}

/// Does a master with these params run confined? Always where this host
/// can confine it — the stored `unconfined` param counts only where it
/// cannot (review round 2).
pub fn is_confined(params: Option<&Value>, available: bool) -> bool {
    available || !unconfined(params)
}

/// The operator's Claude config dir: `CLAUDE_CONFIG_DIR`, else
/// `$HOME/.claude`.
pub fn operator_claude_config(config_dir: Option<String>, home: Option<String>) -> Option<PathBuf> {
    config_dir
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|h| !h.is_empty())
                .map(|h| Path::new(&h).join(".claude"))
        })
}

/// The master's private temp dir (its `TMPDIR`) — `/tmp` itself is not
/// in its confinement.
pub fn tmpdir(state_dir: &Path) -> PathBuf {
    state_dir.join("master").join("tmp")
}

/// The master's working directory: an empty folder under the state dir
/// (review round 1, I4). Never the tracker or a repo — Claude would load
/// a CLAUDE.md, `.mcp.json`, hooks or `.claude/settings*.json` any
/// agent can plant there.
pub fn workdir(state_dir: &Path) -> PathBuf {
    state_dir.join("master").join("cwd")
}

/// `<pm>/agents/<slug>` — the agent's folder.
pub fn agent_dir(pm_dir: &Path, slug: &str) -> PathBuf {
    pm_dir.join("agents").join(slug)
}

fn check_slug(slug: &str) -> Result<()> {
    if slug.is_empty()
        || slug.len() > 64
        || !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(Error::rejected(format!(
            "Bad agent slug '{slug}' — lowercase letters, digits and '-'"
        )));
    }
    Ok(())
}

/// Refuse a name outside [`FILES`] and text over its cap. Refused, never
/// truncated.
pub fn check_file(name: &str, text: &str) -> Result<()> {
    let cap = match name {
        "SOUL.md" => SOUL_MAX_CHARS,
        "AGENT.md" => AGENT_MAX_CHARS,
        other => {
            return Err(Error::rejected(format!(
                "'{other}' is not an agent file — one of {}",
                FILES.join(", ")
            )))
        }
    };
    if text.trim().is_empty() {
        return Err(Error::rejected(format!("{name} is empty")));
    }
    let chars = text.chars().count();
    if chars > cap {
        return Err(Error::rejected(format!(
            "{name} is {chars} characters — the cap is {cap}; shorten it"
        )));
    }
    Ok(())
}

/// The folder, refusing symlinks anywhere on `agents/<slug>` — the
/// writer never follows a link out of the tracker.
fn real_dir(pm_dir: &Path, slug: &str) -> Result<PathBuf> {
    check_slug(slug)?;
    let agents = pm_dir.join("agents");
    let dir = agent_dir(pm_dir, slug);
    for p in [&agents, &dir] {
        if p.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            return Err(Error::rejected(format!(
                "{} is a symlink — agent files are never written through links",
                p.display()
            )));
        }
    }
    Ok(dir)
}

fn default_template(slug: &str, name: &str) -> Option<&'static str> {
    match (slug, name) {
        (ALIAS, "SOUL.md") => Some(SOUL_TEMPLATE),
        (ALIAS, "AGENT.md") => Some(AGENT_TEMPLATE),
        _ => None,
    }
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — refusing to write through it",
            path.display()
        )));
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// Install the repo's default templates for every missing file of the
/// master, in one tracker commit. Existing files are never touched.
/// Returns the names installed (empty when nothing was missing).
pub fn install_defaults(pm: &Pm, actor: &str) -> Result<Vec<String>> {
    let dir = real_dir(&pm.dir, ALIAS)?;
    let _lock = pm.lock()?;
    let missing: Vec<&str> = FILES
        .iter()
        .copied()
        .filter(|name| dir.join(name).symlink_metadata().is_err())
        .collect();
    if missing.is_empty() {
        return Ok(vec![]);
    }
    std::fs::create_dir_all(&dir)?;
    let mut written = Vec::new();
    let undo = |written: &[PathBuf]| {
        for p in written {
            let _ = std::fs::remove_file(p);
        }
    };
    for name in &missing {
        let text = default_template(ALIAS, name).unwrap_or_default();
        let path = dir.join(name);
        if let Err(e) = write_atomic(&path, text) {
            undo(&written);
            return Err(e);
        }
        written.push(path);
    }
    if let Err(e) = crate::issue::write::commit(
        pm,
        &written,
        &format!("agents/{ALIAS}: install default {}", missing.join(", ")),
        &[],
        actor,
    ) {
        undo(&written);
        return Err(e);
    }
    Ok(missing.iter().map(|s| s.to_string()).collect())
}

/// Replace one agent file — the only writer of SOUL.md and AGENT.md.
/// The daemon calls it for the proven operator only. Validated and
/// secret-scanned before anything is written; one tracker commit.
pub fn write_file(pm: &Pm, slug: &str, name: &str, text: &str, actor: &str) -> Result<Value> {
    check_file(name, text)?;
    let warnings = crate::secret::guard(&format!("agents/{slug}/{name}"), text)?;
    let dir = real_dir(&pm.dir, slug)?;
    let _lock = pm.lock()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    let before = std::fs::read(&path).ok();
    write_atomic(&path, text)?;
    if let Err(e) = crate::issue::write::commit(
        pm,
        std::slice::from_ref(&path),
        &format!("agents/{slug}: write {name}"),
        &[],
        actor,
    ) {
        // Put the old file back — a refused commit leaves no write.
        match &before {
            Some(old) => {
                let _ = std::fs::write(&path, old);
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        return Err(e);
    }
    let mut out = json!({
        "agent": slug,
        "file": name,
        "path": path,
        "changed": before.as_deref() != Some(text.as_bytes()),
    });
    if !warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&warnings);
    }
    Ok(out)
}

/// Read the agent's files in [`FILES`] order, refusing symlinks and
/// files over their caps. `all` requires every file; otherwise a
/// missing one is skipped.
pub fn read_files(pm_dir: &Path, slug: &str, all: bool) -> Result<Vec<(String, String)>> {
    let dir = real_dir(pm_dir, slug)?;
    let mut out = Vec::new();
    for name in FILES {
        let path = dir.join(name);
        let meta = path.symlink_metadata();
        if meta.as_ref().is_ok_and(|m| m.is_symlink()) {
            return Err(Error::rejected(format!(
                "{} is a symlink — agent files are never read through links",
                path.display()
            )));
        }
        if meta.is_err() && !all {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::rejected(format!("cannot read {}: {e}", path.display())))?;
        check_file(name, &text)?;
        out.push((name.to_string(), text));
    }
    Ok(out)
}

/// sha256 of one file's text.
pub fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn record_path(state_dir: &Path) -> PathBuf {
    state_dir.join("agent-files.json")
}

fn load_records(state_dir: &Path) -> Value {
    std::fs::read_to_string(record_path(state_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// Remember the digest of `name` the operator's writer (or the
/// installer) left.
pub fn record(state_dir: &Path, slug: &str, name: &str, digest: &str) -> Result<()> {
    let mut all = load_records(state_dir);
    if !all[slug].is_object() {
        all[slug] = json!({});
    }
    all[slug][name] = json!({"digest": digest, "at": crate::issue::time::now_epoch()});
    let path = record_path(state_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&all)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// The launch check: each file must be the one the operator's writer
/// (or the installer) last recorded. It checks every file before it
/// records anything, so a refusal writes nothing. A file with no record
/// yet (placed before this check existed, or just installed) is
/// trusted once and recorded.
pub fn verify(state_dir: &Path, slug: &str, files: &[(String, String)]) -> Result<()> {
    let all = load_records(state_dir);
    let mut unrecorded = Vec::new();
    for (name, text) in files {
        let now = digest(text);
        match all[slug][name]["digest"].as_str() {
            None => unrecorded.push((name, now)),
            Some(known) if known == now => {}
            Some(_) => {
                return Err(Error::invalid(
                    "agent_files_changed",
                    format!(
                        "agents/{slug}/{name} changed outside `cadence master edit` — refusing \
                         to brief the master from it. Review it, then re-save it as the \
                         operator: `cadence master edit {name} --file <path>`"
                    ),
                ))
            }
        }
    }
    for (name, d) in unrecorded {
        record(state_dir, slug, name, &d)?;
    }
    Ok(())
}

/// Longest escalation summary the operator is shown.
pub const ESCALATION_SUMMARY_MAX: usize = 4_000;

fn escalations_path(state_dir: &Path) -> PathBuf {
    state_dir.join("escalations.json")
}

/// The escalations the daemon recorded (CAD-339), keyed
/// `<issue>/<question report>`: `{issue, question, summary, by, at}`.
/// Only the daemon's `question_escalate` writes this file — a report
/// file can never put a question in front of the operator.
pub fn escalations(state_dir: &Path) -> serde_json::Map<String, Value> {
    std::fs::read_to_string(escalations_path(state_dir))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Record one escalation; refuses a question already escalated. The
/// caller (the daemon) serializes writers.
pub fn record_escalation(state_dir: &Path, key: &str, record: Value) -> Result<()> {
    let mut all = escalations(state_dir);
    if all.contains_key(key) {
        return Err(Error::rejected(format!(
            "{key} is already escalated to the operator"
        )));
    }
    all.insert(key.to_string(), record);
    let path = escalations_path(state_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&Value::Object(all))?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// The briefing text: SOUL.md then AGENT.md, verbatim and byte-stable
/// (the prefix caches), under one heading naming their source.
pub fn compose(files: &[(String, String)]) -> String {
    let mut out = String::from(
        "# Master briefing\n\nBuilt by the daemon from agents/master/ (SOUL.md, AGENT.md). \
         Only the operator changes these files.\n",
    );
    for (name, text) in files {
        out.push_str(&format!("\n<!-- agents/master/{name} -->\n"));
        out.push_str(text.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_fit_their_caps() {
        check_file("SOUL.md", SOUL_TEMPLATE).unwrap();
        check_file("AGENT.md", AGENT_TEMPLATE).unwrap();
        assert!(check_file("MEMORY.md", "x").is_err());
        assert!(check_file("SOUL.md", " \n").is_err());
        let long = "x".repeat(SOUL_MAX_CHARS + 1);
        let err = check_file("SOUL.md", &long).unwrap_err().to_string();
        assert!(err.contains("cap is 4000"), "{err}");
    }

    #[test]
    fn verify_trusts_once_then_refuses_a_changed_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let soul = |t: &str| vec![("SOUL.md".to_string(), t.to_string())];
        verify(tmp.path(), "master", &soul("v1")).unwrap();
        verify(tmp.path(), "master", &soul("v1")).unwrap();
        // A newly present file is trusted once; a changed one refuses —
        // and the refusal records nothing for the files beside it.
        let both = vec![
            ("SOUL.md".to_string(), "v2".to_string()),
            ("AGENT.md".to_string(), "a1".to_string()),
        ];
        let err = verify(tmp.path(), "master", &both).unwrap_err().to_string();
        assert!(
            err.contains("agents/master/SOUL.md changed outside"),
            "{err}"
        );
        assert!(load_records(tmp.path())["master"]["AGENT.md"].is_null());
        record(tmp.path(), "master", "SOUL.md", &digest("v2")).unwrap();
        verify(tmp.path(), "master", &both).unwrap();
        assert!(load_records(tmp.path())["master"]["AGENT.md"].is_object());
    }

    #[test]
    fn escalations_record_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(escalations(tmp.path()).is_empty());
        record_escalation(tmp.path(), "D-1/q.md", json!({"summary": "s"})).unwrap();
        let err = record_escalation(tmp.path(), "D-1/q.md", json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("already escalated"), "{err}");
        assert_eq!(escalations(tmp.path())["D-1/q.md"]["summary"], "s");
    }

    #[test]
    fn compose_keeps_file_order_and_text() {
        let files = vec![
            ("SOUL.md".to_string(), "soul text\n".to_string()),
            ("AGENT.md".to_string(), "agent text".to_string()),
        ];
        let text = compose(&files);
        let soul = text.find("soul text").unwrap();
        let agent = text.find("agent text").unwrap();
        assert!(soul < agent, "{text}");
        assert!(text.contains("<!-- agents/master/AGENT.md -->"));
    }

    /// CAD-439: the master's confinement names the system trees, the
    /// Claude CLI's own state and the master's own dirs — never `$HOME`,
    /// the state dir, `/tmp`, `/proc` or `/dev` whole.
    #[test]
    fn confinement_exposes_only_the_masters_views() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("tools").join("claude");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        let policy = confinement(&ConfineInputs {
            state_dir: PathBuf::from("/s"),
            home: Some(PathBuf::from("/h")),
            pm_dir: Some(PathBuf::from("/pm")),
            programs: vec![bin.clone(), PathBuf::from("/missing/cadence")],
            provider_dir: PathBuf::from("/s/master/claude"),
            home_read: CONFINE_HOME_READ,
            extra_read: vec![PathBuf::from("/x")],
            extra_write: vec![PathBuf::from("/y")],
        });
        let all: Vec<&PathBuf> = policy.read.iter().chain(&policy.write).collect();
        for whole in [
            "/", "/h", "/s", "/tmp", "/proc", "/dev", "/home", "/var", "/run",
        ] {
            assert!(
                !all.iter().any(|p| p.as_path() == Path::new(whole)),
                "{whole} exposed whole: {policy:?}"
            );
        }
        let under = |root: &str| -> Vec<String> {
            let mut v: Vec<String> = all
                .iter()
                .filter(|p| p.starts_with(root))
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            v.sort();
            v
        };
        // The operator's Claude config is not in it at all (review I1):
        // only the CLI's install, read-only.
        assert_eq!(under("/h"), ["/h/.local/share/claude"]);
        assert!(policy
            .read
            .contains(&PathBuf::from("/h/.local/share/claude")));
        assert_eq!(
            under("/s"),
            [
                "/s/briefings/master/BRIEFING-master.md",
                "/s/master/claude",
                "/s/master/cwd",
                "/s/master/tmp"
            ]
        );
        // CAD-366: the custody store lives at <state>/custody — never
        // in a confined read set. The full /s list above asserts it,
        // this names the boundary the ADR requires.
        assert!(!all.iter().any(|p| p.starts_with("/s/custody")));
        assert!(!all
            .iter()
            .any(|p| p.starts_with("/dev/pts") || p.ends_with("ptmx")));
        assert_eq!(under("/proc/self"), ["/proc/self"]);
        assert!(policy.write.contains(&PathBuf::from("/pm")));
        assert!(policy.read.contains(&PathBuf::from("/x")));
        assert!(policy.write.contains(&PathBuf::from("/y")));
        // A program's real directory; a missing one adds nothing.
        let dir = bin.canonicalize().unwrap().parent().unwrap().to_path_buf();
        assert!(policy.read.contains(&dir), "{policy:?}");
        assert!(!all.iter().any(|p| p.starts_with("/missing")));
        // The briefing and system trees are read-only.
        assert!(!policy
            .write
            .iter()
            .any(|p| p.starts_with("/usr") || p.starts_with("/etc")));
    }

    /// CAD-322 round 2 (N1): a Pi master's policy is provider-specific —
    /// `master/pi` is writable, `master/claude` (and its
    /// `.credentials.json`) appears in NEITHER set, and none of
    /// `$HOME`'s provider trees (Claude's `~/.local/share/claude`) are
    /// readable. The Claude policy is symmetric in reverse.
    #[test]
    fn pi_confinement_never_sees_the_claude_dirs() {
        let pi = confinement(&ConfineInputs {
            state_dir: PathBuf::from("/s"),
            home: Some(PathBuf::from("/h")),
            pm_dir: None,
            programs: vec![],
            provider_dir: PathBuf::from("/s/master/pi"),
            home_read: &[],
            extra_read: vec![],
            extra_write: vec![],
        });
        let all: Vec<&PathBuf> = pi.read.iter().chain(&pi.write).collect();
        assert!(pi.write.contains(&PathBuf::from("/s/master/pi")));
        for denied in [
            "/s/master/claude",
            "/s/master/claude/.credentials.json",
            "/h/.local/share/claude",
        ] {
            assert!(
                !all.iter().any(|p| p.starts_with(denied)),
                "pi policy reaches {denied}: {pi:?}"
            );
        }
        assert!(!all.iter().any(|p| p.starts_with("/h")), "{pi:?}");
        // And Claude's own policy answers in kind: no `master/pi` in it.
        let claude = confinement(&ConfineInputs {
            state_dir: PathBuf::from("/s"),
            home: None,
            pm_dir: None,
            programs: vec![],
            provider_dir: PathBuf::from("/s/master/claude"),
            home_read: CONFINE_HOME_READ,
            extra_read: vec![],
            extra_write: vec![],
        });
        assert!(claude.write.contains(&PathBuf::from("/s/master/claude")));
        assert!(!claude
            .read
            .iter()
            .chain(&claude.write)
            .any(|p| p.starts_with("/s/master/pi")));
    }

    /// Review I2: a `cadence` release grants the whole releases root, so
    /// an upgrade repointing the link mid-run keeps the verbs runnable.
    #[test]
    fn a_cadence_release_grants_its_releases_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rel = tmp.path().join("releases");
        let sha = "a".repeat(40);
        let bin = rel.join(&sha).join("cadence");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "").unwrap();
        let link = tmp.path().join("cadence");
        std::os::unix::fs::symlink(&bin, &link).unwrap();
        let policy = confinement(&ConfineInputs {
            state_dir: PathBuf::from("/s"),
            home: None,
            pm_dir: None,
            programs: vec![link],
            provider_dir: PathBuf::from("/s/master/claude"),
            home_read: CONFINE_HOME_READ,
            extra_read: vec![],
            extra_write: vec![],
        });
        let root = rel.canonicalize().unwrap();
        assert!(policy.read.contains(&root), "{policy:?}");
        assert!(!policy.read.iter().any(|p| p.starts_with(root.join(&sha))));
    }

    /// Operator decision: by default nothing is copied — the dir is made
    /// 0700 (tightened when it already exists, review round 2) and the
    /// login is reported missing, with the command that creates one.
    #[test]
    fn config_dir_is_0700_and_holds_no_copied_login_by_default() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let state = tmp.path().join("s");
        let dir = claude_config_dir(&state);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(ensure_config_dir(&state).unwrap(), Login::None);
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert!(!has_login(&state));
        assert_eq!(
            login_command(&state),
            format!("CLAUDE_CONFIG_DIR={} claude auth login", dir.display())
        );
        std::fs::write(dir.join(".credentials.json"), "{}").unwrap();
        assert_eq!(ensure_config_dir(&state).unwrap(), Login::Own);
    }

    /// Review round 2: the stored `unconfined` param counts only where
    /// the host cannot confine.
    #[test]
    fn unconfined_param_counts_only_without_landlock() {
        let loose = json!({"unconfined": true});
        assert!(is_confined(Some(&loose), true));
        assert!(!is_confined(Some(&loose), false));
        assert!(is_confined(Some(&json!({})), false));
        assert!(is_confined(None, true));
    }

    /// `--copy-login`: the operator's `claudeAiOauth` only (never its
    /// MCP tokens), 0600 in a 0700 dir, written once.
    #[test]
    fn provision_login_copies_only_the_claude_login_once() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let (state, operator) = (tmp.path().join("s"), tmp.path().join("op"));
        std::fs::create_dir_all(&operator).unwrap();
        assert_eq!(copy_login(&state, &operator).unwrap(), Login::None);
        // Secret-shaped values built at runtime.
        let token = format!("tok-{}", uuid::Uuid::new_v4().simple());
        let mcp = format!("mcp-{}", uuid::Uuid::new_v4().simple());
        let creds = json!({
            "claudeAiOauth": {"accessToken": token, "refreshToken": "r", "expiresAt": 1},
            "mcpOAuth": {"github|x": {"accessToken": mcp}},
        });
        std::fs::write(operator.join(".credentials.json"), creds.to_string()).unwrap();
        assert_eq!(copy_login(&state, &operator).unwrap(), Login::Copied);
        let file = claude_config_dir(&state).join(".credentials.json");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(&token) && !text.contains(&mcp), "{text}");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&claude_config_dir(&state)), 0o700);
        // An existing login — the master's own sign-in — is left alone.
        std::fs::write(&file, "{\"claudeAiOauth\":{\"accessToken\":\"own\"}}").unwrap();
        assert_eq!(copy_login(&state, &operator).unwrap(), Login::Own);
        assert!(std::fs::read_to_string(&file).unwrap().contains("own"));
        // A credentials file without a Claude login copies nothing.
        let other = tmp.path().join("s2");
        std::fs::write(operator.join(".credentials.json"), "{\"mcpOAuth\":{}}").unwrap();
        assert_eq!(copy_login(&other, &operator).unwrap(), Login::None);
        assert!(!claude_config_dir(&other).join(".credentials.json").exists());
    }

    #[test]
    fn interpreter_of_a_script_is_a_program_too() {
        let tmp = tempfile::TempDir::new().unwrap();
        let node = tmp.path().join("node");
        std::fs::write(&node, "").unwrap();
        std::fs::set_permissions(&node, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let cli = tmp.path().join("cli.js");
        std::fs::write(&cli, "#!/usr/bin/env node\nconsole.log(1)\n").unwrap();
        let path = tmp.path().to_str().unwrap();
        assert_eq!(with_interpreter(&cli, Some(path)), [cli.clone(), node]);
        let sh = tmp.path().join("run.sh");
        std::fs::write(&sh, "#!/bin/bash -e\n").unwrap();
        assert_eq!(
            with_interpreter(&sh, None),
            [sh.clone(), PathBuf::from("/bin/bash")]
        );
        assert_eq!(which("node", Some(path)), Some(tmp.path().join("node")));
        assert_eq!(which("nope", Some(path)), None);
        assert_eq!(
            split_paths(Some("/a::/b".into())),
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn slugs_and_symlinks_are_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(real_dir(tmp.path(), "../x").is_err());
        assert!(real_dir(tmp.path(), "Master").is_err());
        std::fs::create_dir_all(tmp.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("elsewhere"), tmp.path().join("agents"))
            .unwrap();
        let err = real_dir(tmp.path(), "master").unwrap_err().to_string();
        assert!(err.contains("symlink"), "{err}");
    }
}
