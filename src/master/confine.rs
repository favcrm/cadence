//! Filesystem confinement, executable/interpreter discovery, and provider
//! login/config helpers for [`super`].

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

#[cfg(debug_assertions)]
use crate::error::Error;
use crate::error::Result;

use super::{tmpdir, workdir, ALIAS};

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

/// CAD-1287: where systemd-resolved keeps the stub `/etc/resolv.conf`
/// points at on stock Ubuntu; `/etc` alone does not cover it.
const RESOLVED_DIR: &str = "/run/systemd/resolve";

/// The resolver directory the master may also read: exactly
/// [`RESOLVED_DIR`], and only when `resolv_conf` is a symlink whose
/// canonical target is inside it. A regular file, any other target, a
/// dangling link or any resolution error grants nothing.
pub(super) fn resolver_read(resolv_conf: &Path) -> Option<PathBuf> {
    let meta = std::fs::symlink_metadata(resolv_conf).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    resolver_grant(&std::fs::canonicalize(resolv_conf).ok()?)
}

pub(super) fn resolver_grant(canonical: &Path) -> Option<PathBuf> {
    let dir = Path::new(RESOLVED_DIR);
    let inside = canonical.starts_with(dir)
        && canonical != dir
        && canonical.components().all(|c| c != Component::ParentDir);
    inside.then(|| dir.to_path_buf())
}

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
    read.extend(resolver_read(Path::new("/etc/resolv.conf")));
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
pub(crate) fn program_dirs(program: &Path) -> Vec<PathBuf> {
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
/// decision, 2026-09-24: a separate login is the default), or the same in words
/// when the state dir cannot be printed as a safe command.
pub fn login_command(state_dir: &Path) -> String {
    use crate::printed_command::{bare_command, Part};
    bare_command(&[
        Part::Fixed("CLAUDE_CONFIG_DIR="),
        Part::Path(&claude_config_dir(state_dir)),
        Part::Fixed(" claude auth login"),
    ])
    .unwrap_or_else(|| {
        "Set CLAUDE_CONFIG_DIR to master/claude under the state dir, whose path cannot be \
         printed as a safe command, then run claude auth login"
            .to_string()
    })
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

/// The command that gives the master its own login for `provider`, or the same in
/// words when the state dir cannot be printed as a safe command.
/// Pi has no non-interactive login verb — the operator runs the TUI
/// against the master's config dir and types `/login`.
pub fn login_command_for(provider: &str, state_dir: &Path) -> String {
    match provider {
        "pi" => {
            use crate::printed_command::{bare_command, Part};
            bare_command(&[
                Part::Fixed("PI_CODING_AGENT_DIR="),
                Part::Path(&provider_config_dir("pi", state_dir)),
                Part::Fixed(" pi  # then type /login"),
            ])
            .unwrap_or_else(|| {
                "Set PI_CODING_AGENT_DIR to master/pi under the state dir, whose path cannot be \
                 printed as a safe command, then run pi and type /login"
                    .to_string()
            })
        }
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
