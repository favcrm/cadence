//! Pi confinement: master/worker/assistant sandbox profiles and their refusals.

use super::*;

/// The master, confined wherever this host can confine it — the stored
/// `unconfined` param counts only where it cannot (same rule as Claude).
pub(super) fn master_confined(env: &ProviderEnv, agent: &Agent) -> bool {
    crate::master::is_master(&agent.alias)
        && crate::master::is_confined(
            agent.params.as_ref(),
            crate::master::confinement_available(env).is_ok(),
        )
}

/// The confining binary and policy for a Pi master (`cadence master
/// confinement` prints the Claude one; Pi's differs only in the
/// programs and provider dirs it names).
pub fn pi_master_confinement(
    env: &ProviderEnv,
    state_dir: &Path,
) -> (String, crate::confine::Policy) {
    let command = pi_command(env);
    (
        confine_command(env),
        crate::master::confinement(&pi_confine_inputs(env, state_dir, &command)),
    )
}

pub(super) fn confine_command(env: &ProviderEnv) -> String {
    #[cfg(debug_assertions)]
    let own = env
        .own("CADENCE_CONFINE_COMMAND")
        .filter(|c| !c.trim().is_empty());
    #[cfg(not(debug_assertions))]
    let own: Option<String> = {
        let _ = env;
        None
    };
    own.or_else(|| {
        std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    })
    .unwrap_or_else(|| "cadence".to_string())
}

/// The directory an npm-installed package lives in: `program`'s real
/// path's first ancestor holding a `package.json` within its enclosing
/// `node_modules` tree. Unrelated ancestor manifests are not install roots.
/// Pi is a node script
/// inside `…/node_modules/@earendil-works/pi-coding-agent`, so the bin
/// symlink's parent alone is not enough — the package dir is added to
/// the master's read set.
pub(super) fn package_root(program: &Path) -> Option<PathBuf> {
    let real = program.canonicalize().ok()?;
    let node_modules = real
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "node_modules"))?;
    real.ancestors()
        .take_while(|dir| *dir != node_modules)
        .find(|dir| dir.join("package.json").is_file())
        .map(Path::to_path_buf)
}

/// Read a small regular file without following a final-component symlink
/// and with a hard allocation/read cap, even if it changes during the read.
pub(super) fn read_bounded_regular_file(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let before = std::fs::symlink_metadata(path).ok()?;
    if !before.file_type().is_file() || before.len() > max_bytes {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let after = file.metadata().ok()?;
    if !after.file_type().is_file() || after.len() > max_bytes {
        return None;
    }
    let mut bytes = Vec::with_capacity(max_bytes as usize);
    file.take(max_bytes + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= max_bytes).then_some(bytes)
}

/// Resolve only the known managed launcher layout. The wrapper is never
/// executed during policy discovery: its version pointer and expected
/// release paths are read and checked as data instead.
pub(super) fn managed_pi_dependencies(program: &Path, env: &ProviderEnv) -> Vec<PathBuf> {
    let Some(real) = program.canonicalize().ok() else {
        return Vec::new();
    };
    let Some(bin) = real.parent() else {
        return Vec::new();
    };
    if real.file_name().is_none_or(|name| name != "pi")
        || bin.file_name().is_none_or(|name| name != "bin")
    {
        return Vec::new();
    }
    let Some(agent_root) = bin.parent() else {
        return Vec::new();
    };
    if agent_root.file_name().is_none_or(|name| name != "agent")
        || agent_root
            .parent()
            .and_then(Path::file_name)
            .is_none_or(|name| name != ".pi")
    {
        return Vec::new();
    }
    // A managed install has exactly <agent>/bin/pi and a sibling
    // <agent>/install tree. Do not infer this from an arbitrary script.
    if agent_root
        .join("bin")
        .join("pi")
        .canonicalize()
        .ok()
        .as_deref()
        != Some(real.as_path())
    {
        return Vec::new();
    }
    let install = agent_root.join("install");
    let Ok(real_install) = install.canonicalize() else {
        return Vec::new();
    };
    if real_install != install || !real_install.is_dir() {
        return Vec::new();
    }

    let pointer = install.join("current-version");
    const MAX_POINTER_BYTES: u64 = 130; // 128 safe version bytes plus CRLF.
    let Some(pointer_bytes) = read_bounded_regular_file(&pointer, MAX_POINTER_BYTES) else {
        return Vec::new();
    };
    let Ok(contents) = std::str::from_utf8(&pointer_bytes) else {
        return Vec::new();
    };
    let version = contents
        .strip_suffix("\r\n")
        .or_else(|| contents.strip_suffix('\n'))
        .unwrap_or(contents);
    if version.is_empty()
        || version.len() > 128
        || version == "."
        || version == ".."
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
    {
        return Vec::new();
    }

    let releases = install.join("releases");
    let Ok(real_releases) = releases.canonicalize() else {
        return Vec::new();
    };
    if real_releases != releases || !real_releases.is_dir() {
        return Vec::new();
    }
    let release = releases.join(version);
    let Ok(real_release) = release.canonicalize() else {
        return Vec::new();
    };
    // Reject a version entry that is a symlink or escapes the exact
    // current-release directory; never grant a neighboring release.
    if real_release != release || real_release.parent() != Some(real_releases.as_path()) {
        return Vec::new();
    }
    let executable = release.join("node_modules/.bin/pi");
    let Some(package) = package_root(&executable) else {
        return Vec::new();
    };
    let Ok(real_package) = package.canonicalize() else {
        return Vec::new();
    };
    if !real_package.starts_with(&real_release)
        || executable
            .canonicalize()
            .ok()
            .is_none_or(|target| !target.starts_with(&real_release))
    {
        return Vec::new();
    }
    let manifest = real_package.join("package.json");
    let Some(manifest_bytes) = read_bounded_regular_file(&manifest, 1024 * 1024) else {
        return Vec::new();
    };
    let Ok(manifest_json) = serde_json::from_slice::<serde_json::Value>(&manifest_bytes) else {
        return Vec::new();
    };
    if manifest_json
        .get("name")
        .and_then(serde_json::Value::as_str)
        != Some("@earendil-works/pi-coding-agent")
    {
        return Vec::new();
    }

    let mut roots = vec![pointer, real_release];
    // The managed wrapper uses ${XDG_DATA_HOME:-$HOME/.local/share} for
    // its optional Node. Match that environment, then accept only a direct
    // version child of the canonical pi-node anchor.
    let data_home = env
        .var("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env.var("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/share"))
        });
    if let Some(data_home) = data_home {
        if let Ok(real_data_home) = data_home.canonicalize() {
            let node_root = real_data_home.join("pi-node");
            if let (Ok(node_root_meta), Ok(real_node_root)) = (
                std::fs::symlink_metadata(&node_root),
                node_root.canonicalize(),
            ) {
                if node_root_meta.file_type().is_dir() && real_node_root == node_root {
                    let node = node_root.join("current/bin/node");
                    if let Ok(real_node) = node.canonicalize() {
                        if let Some(node_version) = real_node
                            .parent()
                            .and_then(Path::parent)
                            .map(Path::to_path_buf)
                        {
                            if let Ok(real_node_version) = node_version.canonicalize() {
                                use std::os::unix::fs::PermissionsExt;
                                if real_node.is_file()
                                    && real_node_version.parent() == Some(real_node_root.as_path())
                                    && real_node
                                        .metadata()
                                        .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
                                    && real_node_version
                                        .join("bin/node")
                                        .canonicalize()
                                        .ok()
                                        .as_deref()
                                        == Some(real_node.as_path())
                                {
                                    roots.push(real_node_version);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    roots
}

/// What the Pi master's confinement is computed from — same shape as
/// Claude's inputs, plus Pi's package dir, the guard extension and its
/// private `PI_CODING_AGENT_DIR`.
pub(super) fn pi_confine_inputs(
    env: &ProviderEnv,
    state_dir: &Path,
    command: &[String],
) -> crate::master::ConfineInputs {
    use crate::master::{split_paths, which, with_interpreter};
    let path = env.var("PATH");
    let mut programs = Vec::new();
    if let Some(p) = command.first().and_then(|c| which(c, path.as_deref())) {
        programs.extend(with_interpreter(&p, path.as_deref()));
        if let Some(root) = package_root(&p) {
            programs.push(root);
        }
    }
    if let Some(p) = which("cadence", path.as_deref()) {
        programs.push(p);
    }
    programs.push(PathBuf::from(confine_command(env)));
    let pm_dir = pi_pm_dir(env);
    // The guard lives outside `master/pi` — a confined master must read
    // but never write it. Its own provider dir is `master/pi`: nothing
    // of `master/claude` (or its `.credentials.json`) is in the policy.
    let mut extra_read = split_paths(env.var(crate::master::CONFINE_EXTRA_READ_ENV));
    if let Some(program) = command.first().and_then(|c| which(c, path.as_deref())) {
        extra_read.extend(managed_pi_dependencies(&program, env));
    }
    extra_read.push(pi_guard_path(state_dir));
    if let Ok(path) = pi_turn_guard_path(state_dir, crate::master::ALIAS) {
        extra_read.push(path);
    }
    if let Ok(Some(path)) = agenticos_read_extension(env) {
        extra_read.push(path);
    }
    // CAD-559: exactly the pinned provider package dirs — read-only,
    // plus each package's installed runtime dependency dirs (CAD-1449:
    // without them the extension's `require`/`import` fails
    // MODULE_NOT_FOUND inside the sandbox). `build_command` resolves
    // the same list and refuses the launch on a bad pin or closure,
    // so a policy with an unresolvable package never runs.
    if let Ok(packages) = provider_packages(env) {
        for pkg in &packages {
            extra_read.push(pkg.dir.clone());
            extra_read.extend(pkg.deps.iter().cloned());
        }
    }
    crate::master::ConfineInputs {
        state_dir: state_dir.to_path_buf(),
        home: env.var("HOME").filter(|h| !h.is_empty()).map(PathBuf::from),
        pm_dir,
        programs,
        provider_dir: pi_config_dir(state_dir),
        // Nothing of the operator's `$HOME` — Pi's npm package arrives
        // via `programs`; Claude's `~/.local/share/claude` is not ours.
        home_read: &[],
        extra_read,
        extra_write: split_paths(env.var(crate::master::CONFINE_EXTRA_WRITE_ENV)),
    }
}

/// Daemon env naming extra paths (`:`-separated) a pi WORKER may read,
/// or also write — the escape hatch for caches and push targets the
/// built-in policy does not know (a `file://` remote, a private
/// registry dir, a uv/npm cache). Set by whoever starts the daemon,
/// never by an agent.
pub const CONFINE_WORKER_EXTRA_READ_ENV: &str = "CADENCE_PI_WORKER_CONFINE_READ";
pub const CONFINE_WORKER_EXTRA_WRITE_ENV: &str = "CADENCE_PI_WORKER_CONFINE_WRITE";

/// Is `agent` a worker launched confined? Opt-in (`params.confine`,
/// CAD-556 — `join … pi --confine` or `pm.yaml [host]
/// confine_pi_workers`); the master alias's posture is `is_confined`,
/// not this. Availability is checked by the caller. `agent_show`
/// reports this verbatim as `confined`.
pub fn worker_confined(agent: &Agent) -> bool {
    !crate::master::is_master(&agent.alias)
        && agent
            .params
            .as_ref()
            .and_then(|p| p.get("confine"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// The directory git uses for object/ref writes — the worktree's own
/// `.git` for a plain clone, the main checkout's `.git` for a linked
/// worktree (`git rev-parse --git-common-dir`). `None` when the cwd is
/// not a repo (the policy then just grants the cwd).
pub(super) fn git_common_dir(cwd: &Path) -> Option<PathBuf> {
    let out = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["rev-parse", "--git-common-dir"]),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let dir = PathBuf::from(&text);
    let dir = if dir.is_absolute() {
        dir
    } else {
        cwd.join(dir)
    };
    Some(dir.canonicalize().unwrap_or(dir))
}

/// CAD-1752: the directory a Pi launch starts in. `agent.cwd` is the
/// agent's identity (a catalogue hire's home dir); a catalogue agent that
/// was relaunched for a ticket carries the daemon-validated lane worktree
/// in `params.launch_workdir`. Nobody else's launch reads that key, and
/// registration and `agent set` refuse it from callers.
pub fn launch_workdir(agent: &Agent) -> PathBuf {
    let params = agent.params.as_ref();
    let is_hire = crate::agent_roles::is_catalogue_hire(params);
    match params
        .and_then(|p| p.get("launch_workdir"))
        .and_then(Value::as_str)
        .filter(|_| is_hire)
    {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(&agent.cwd),
    }
}

/// The confining binary and policy for a pi WORKER (CAD-556) — the
/// same `cadence confine` launcher as the master, a wider but still
/// enumerated set. A worker edits, builds, commits and reports:
///
/// - **write**: its worktree (`agent.cwd`); the repo's shared git dir
///   when the worktree links one (objects, refs, the per-worktree
///   index); its cargo target dir ([`crate::worktree::
///   effective_target_dir`] — the declared `--target-dir`/
///   `CARGO_TARGET_DIR`/config value) plus the shared dep cache
///   ([`crate::worktree::shared_target_dir`]) the worktree's target
///   symlinks into; its own dir under the state dir (`pi/` config —
///   including `pi/cache`, the agent's own XDG_CACHE_HOME (CAD-570) —
///   session file, `tmp/` — the confined child's TMPDIR is redirected
///   there because a `/tmp` write grant would expose every lane);
///   the PM tracker (`issue` verbs commit to it); the cargo
///   `registry`/`git` caches and their lock files (a worker fetches
///   and updates deps — proven: cargo fails building without them);
///   the sccache dir (`rustc-wrapper` writes miss entries).
/// - **read**: the master's system trees; the programs the launch
///   resolves (pi, its interpreter and npm package dir, `cadence`,
///   the `confine` exe); its briefing file and the sandbox marker;
///   `RUSTUP_HOME` (toolchain bins execute from it) and
///   `$CARGO_HOME/bin` (the rustup shims on PATH); `~/.ssh/config` and
///   `~/.ssh/known_hosts` — the two NON-secret ssh files, so `git
///   push` over ssh + agent works while the keys themselves stay
///   denied; `$CARGO_HOME/config.toml` — cargo aborts the whole run on
///   an unreadable config (proven), and this file carries the
///   rustc-wrapper/source-mirror settings builds need. Cargo's
///   `credentials.toml` (registry tokens) and the Devin CLI login
///   (`~/.local/share/devin/credentials.toml`) are never granted.
///   The pinned `[pi].providers`
///   package dirs the `-e` argv loads (CAD-559).
/// - **denied** by omission: `$HOME` itself and everything under it
///   not named above — `~/.ssh` keys, `~/.pi`, `~/.claude`, other
///   agents' dirs under the state dir (`<state>/agents/<alias>` is the
///   only granted one), the daemon store. Network is not covered by
///   Landlock — unchanged from the unconfined posture (the provider's
///   API and git remotes must still be reachable).
///
/// `GIT_CONFIG_GLOBAL` is redirected to the worker's own file by
/// `open` (git fatals on an unreadable `~/.gitconfig` — proven).
///
/// A confined managed Pi worker cannot use the Devin provider: pi-devin
/// reads the operator's login file on every turn, and granting that file
/// to a worker would let its tools copy or transmit the credential.
/// Call this before persisting a registration, changing a launch model,
/// dispatching work, and opening an existing row.
pub fn refuse_confined_devin(alias: &str, confined: bool, model: &str) -> Result<()> {
    if confined && model.starts_with("devin/") {
        return Err(Error::rejected(format!(
            "pi worker '{alias}' cannot use model '{model}' while confined: \
             pi-devin requires the operator's Devin credential, which a confined \
             worker cannot safely read. Choose a non-Devin model or have the \
             operator register an unconfined worker (CAD-751)"
        )));
    }
    Ok(())
}

pub fn refuse_confined_devin_agent(agent: &Agent) -> Result<()> {
    if crate::adapter::registry::pi_family_managed(&agent.provider, &agent.endpoint_kind)
        && !crate::master::is_master(&agent.alias)
    {
        let params = agent.params.as_ref().unwrap_or(&Value::Null);
        refuse_confined_devin(
            &agent.alias,
            params.get("confine").and_then(Value::as_bool) == Some(true),
            params
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )?;
    }
    Ok(())
}
pub fn pi_worker_confinement(
    env: &ProviderEnv,
    state_dir: &Path,
    agent: &Agent,
) -> (String, crate::confine::Policy) {
    use crate::master::{program_dirs, split_paths, which, with_interpreter};
    let path = env.var("PATH");
    let home = env.var("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    let command = pi_command(env);
    let cwd = launch_workdir(agent);

    let mut read: Vec<PathBuf> = crate::master::CONFINE_SYSTEM_READ
        .iter()
        .map(PathBuf::from)
        .collect();
    let mut write: Vec<PathBuf> = crate::master::CONFINE_SYSTEM_WRITE
        .iter()
        .map(PathBuf::from)
        .collect();

    // Programs the launch line and the worker's own commands resolve:
    // pi + its interpreter/package, cadence (the CLI a worker calls:
    // `message result`, `issue`), the `confine` exe that wraps it all.
    let mut programs = Vec::new();
    if let Some(p) = command.first().and_then(|c| which(c, path.as_deref())) {
        programs.extend(with_interpreter(&p, path.as_deref()));
        if let Some(root) = package_root(&p) {
            programs.push(root);
        }
        // CAD-1822: a managed Pi launcher resolves its version pointer,
        // selected release and selected Node from the installed tree before
        // the provider starts. Reuse the same validated read-only dependency
        // list the master policy grants; neighboring releases, credentials
        // and the operator's HOME stay denied.
        for dependency in managed_pi_dependencies(&p, env) {
            if !read.contains(&dependency) {
                read.push(dependency);
            }
        }
    }
    if let Some(p) = which("cadence", path.as_deref()) {
        programs.push(p);
    }
    programs.push(PathBuf::from(confine_command(env)));
    for program in &programs {
        for dir in program_dirs(program) {
            if !read.contains(&dir) {
                read.push(dir);
            }
        }
    }

    let params = agent.params.clone().unwrap_or(Value::Null);
    if let Ok(path) = pi_turn_guard_path(state_dir, &agent.alias) {
        read.push(path);
    }
    read.push(crate::client::briefing_path(
        state_dir,
        &params,
        &agent.alias,
    ));
    if let Some(marker) = crate::sandbox::marker_for(state_dir) {
        read.push(marker);
    }

    // The worktree and everything a commit/build writes into or
    // through: the common git dir (objects, refs, per-worktree index),
    // the effective target dir (covers `build.target-dir` and a
    // declared `CARGO_TARGET_DIR`), and the shared dep cache the
    // worktree's target links into (`<root>/.cadence/target/shared`).
    write.push(cwd.clone());
    if let Some(common) = git_common_dir(&cwd) {
        write.push(common.clone());
        if let Some(root) = common
            .parent()
            .filter(|_| common.file_name().is_some_and(|n| n == ".git"))
        {
            write.push(crate::worktree::shared_target_dir(root));
        }
    }
    let target = crate::worktree::effective_target_dir(&cwd);
    if target != cwd.join("target") {
        write.push(target);
    }

    // Private state: the worker's own dir (config, session, tmp),
    // the tracker, and the declared toolchain caches.
    if let Ok(dir) = pi_worker_dir(state_dir, &agent.alias) {
        write.push(dir);
    }
    let pm_dir = env
        .var("CADENCE_PM_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::issue::default_dir().ok());
    if let Some(pm) = pm_dir {
        write.push(pm);
    }
    let cargo_home = env
        .var("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".cargo")));
    if let Some(cargo) = &cargo_home {
        read.push(cargo.join("bin"));
        // `config.toml` — cargo dies on EACCES (proven); the rustc-
        // wrapper and source mirrors live here. `credentials.toml`
        // (registry tokens) is deliberately NOT granted.
        read.push(cargo.join("config.toml"));
        for cache in ["registry", "git", ".global-cache"] {
            write.push(cargo.join(cache));
        }
        write.push(cargo.join(".package-cache"));
    }
    let rustup_home = env
        .var("RUSTUP_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".rustup")));
    if let Some(rustup) = rustup_home {
        read.push(rustup);
    }
    let sccache = env
        .var("SCCACHE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".cache/sccache")));
    if let Some(dir) = sccache {
        write.push(dir);
    }
    if let Some(home) = &home {
        // sccache's own config (`~/.config/sccache/config`) — settings,
        // not secrets. The ssh files are the non-secret pair only:
        // keys under `~/.ssh` stay denied; agent auth rides
        // SSH_AUTH_SOCK, a unix-socket connect Landlock does not gate.
        read.push(home.join(".config/sccache"));
        read.push(home.join(".ssh/config"));
        read.push(home.join(".ssh/known_hosts"));
        // Devin login remains outside every worker's filesystem policy.
    }
    // CAD-570: the worker's own XDG_CACHE_HOME (`<worker>/pi/cache`),
    // named explicitly so the write set shows it — it is inside the
    // `agents/<alias>` grant either way, so the push is documentation
    // plus resilience if that grant ever narrows.
    if let Ok(cache) = pi_worker_config_dir(state_dir, &agent.alias).map(|d| d.join("cache")) {
        if !write.contains(&cache) {
            write.push(cache);
        }
    }
    // Pinned provider packages and their validated runtime dependencies are
    // read-only, as in the master policy. `build_command` refuses a bad pin
    // or dependency closure before this policy can be used.
    if let Ok(packages) = provider_packages(env) {
        for pkg in packages {
            if !read.contains(&pkg.dir) {
                read.push(pkg.dir);
            }
            for dep in pkg.deps {
                if !read.contains(&dep) {
                    read.push(dep);
                }
            }
        }
    }
    // CAD-1822: a managed worker's Node resolves names through the system
    // resolver. `/etc/resolv.conf` may point at a systemd-resolved file
    // outside the base read set; grant ONLY the canonical selected regular
    // file from the two public resolver targets — never the directory, a
    // sibling, an arbitrary symlink target or a private file. A plain
    // regular `/etc/resolv.conf` is already covered by the system read set.
    if let Ok(selected) = std::fs::canonicalize("/etc/resolv.conf") {
        const RESOLVER_TARGETS: &[&str] = &[
            "/run/systemd/resolve/stub-resolv.conf",
            "/run/systemd/resolve/resolv.conf",
        ];
        if selected.is_file()
            && RESOLVER_TARGETS
                .iter()
                .any(|allowed| selected.as_path() == Path::new(allowed))
            && !read.contains(&selected)
        {
            read.push(selected);
        }
    }
    read.extend(split_paths(env.var(CONFINE_WORKER_EXTRA_READ_ENV)));
    write.extend(split_paths(env.var(CONFINE_WORKER_EXTRA_WRITE_ENV)));
    (confine_command(env), crate::confine::Policy { read, write })
}

/// CAD-1380: the member-assistant confinement policy — the narrowest
/// worker profile. The caller-bound assistant (`params.assist` = the
/// member's `sub`, set by the daemon's own `ensure_assistant_runtime`,
/// never a request field) gets:
///
/// - **write**: ONLY its own principal-owned cwd
///   (`state_dir/assistants/<sub>` — the registered `agent.cwd`) and
///   its private worker dir under the state dir (config, session, tmp).
///   No PM tracker (`pm_dir`), no worktree/git dir, no cargo/sccache
///   caches — a member assistant never writes a lane's issue, a build
///   artifact or a shared dep cache.
/// - **read**: the system trees, the launch programs (pi, cadence,
///   confine), its briefing file and sandbox marker, its own turn
///   guard, the pinned `[pi].providers` package dirs, and its own cwd.
///   No `$HOME` reads at all — no ssh files, no `.config`, no cargo
///   config: a member assistant does not push over ssh or compile, so
///   the non-secret grants the worker carries are dropped wholesale.
///   The daemon store and every other member's `assistants/<sub>` dir
///   stay denied by omission.
///
/// The policy is derived from the daemon-owned registration —
/// `agent.cwd` is the subdir `ensure_assistant_runtime` created — so an
/// immutable `(org, sub)` binding, not a caller field, picks it.
pub(super) fn pi_assistant_confinement(
    env: &ProviderEnv,
    state_dir: &Path,
    agent: &Agent,
) -> (String, crate::confine::Policy) {
    use crate::master::{program_dirs, which, with_interpreter};
    let path = env.var("PATH");
    let command = pi_command(env);
    let cwd = PathBuf::from(&agent.cwd);

    let mut read: Vec<PathBuf> = crate::master::CONFINE_SYSTEM_READ
        .iter()
        .map(PathBuf::from)
        .collect();
    let mut write: Vec<PathBuf> = crate::master::CONFINE_SYSTEM_WRITE
        .iter()
        .map(PathBuf::from)
        .collect();

    // Programs only: pi + its interpreter/package, cadence (the
    // assistant's own `message report`/`thread read`), confine.
    let mut programs = Vec::new();
    if let Some(p) = command.first().and_then(|c| which(c, path.as_deref())) {
        programs.extend(with_interpreter(&p, path.as_deref()));
        if let Some(root) = package_root(&p) {
            programs.push(root);
        }
    }
    if let Some(p) = which("cadence", path.as_deref()) {
        programs.push(p);
    }
    programs.push(PathBuf::from(confine_command(env)));
    for program in &programs {
        for dir in program_dirs(program) {
            if !read.contains(&dir) {
                read.push(dir);
            }
        }
    }

    let params = agent.params.clone().unwrap_or(Value::Null);
    if let Ok(path) = pi_turn_guard_path(state_dir, &agent.alias) {
        read.push(path);
    }
    read.push(crate::client::briefing_path(
        state_dir,
        &params,
        &agent.alias,
    ));
    if let Some(marker) = crate::sandbox::marker_for(state_dir) {
        read.push(marker);
    }
    // Read its own cwd (the member's principal-owned dir) so the
    // assistant reads what it wrote; write it too.
    read.push(cwd.clone());
    write.push(cwd);
    // Its private worker dir under the state dir (config/session/tmp).
    if let Ok(dir) = pi_worker_dir(state_dir, &agent.alias) {
        write.push(dir);
    }
    if let Ok(cache) = pi_worker_config_dir(state_dir, &agent.alias).map(|d| d.join("cache")) {
        write.push(cache);
    }
    // Pinned `[pi].providers` package dirs, read-only.
    if let Ok(packages) = provider_packages(env) {
        for pkg in packages {
            if !read.contains(&pkg.dir) {
                read.push(pkg.dir);
            }
        }
    }
    (confine_command(env), crate::confine::Policy { read, write })
}

/// Resolve the operator's git identity (`user.name`, `user.email`)
/// into the worker's private `gitconfig` — the file
/// `GIT_CONFIG_GLOBAL` points at once confined. Written only when the
/// operator has an identity to copy; an absent file reads as empty
/// config, so a repo-local `git config user.*` or none is honoured the
/// same way an unconfined worker would.
pub(super) fn seed_worker_gitconfig(dir: &Path, env: &ProviderEnv) -> Result<()> {
    let Some(home) = env.var("HOME").filter(|h| !h.is_empty()) else {
        return Ok(());
    };
    let get = |key: &str| {
        crate::reaper::output(
            std::process::Command::new("git")
                .env("HOME", &home)
                .args(["config", "--global", "--get", key]),
        )
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
    };
    let (Some(name), Some(email)) = (get("user.name"), get("user.email")) else {
        return Ok(());
    };
    std::fs::write(
        dir.join("gitconfig"),
        format!("[user]\n\tname = {name}\n\temail = {email}\n"),
    )?;
    Ok(())
}
