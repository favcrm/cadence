//! Path confinement, private directory creation, and Master workspace isolation guards.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};

use super::{guard_home, layout, user_home, Source};
use crate::error::{Error, Result};

/// Refuse a root whose existing ancestry contains a symlink or a `..`.
/// An explicit `CADENCE_HOME` is authoritative for credentials, so a
/// planted link anywhere above it would redirect every write even when the
/// leaf itself is not a link. Nothing below the first missing component is
/// checked; the caller then creates it under the refusal rules above.
pub(super) fn refuse_symlinked_ancestry(root: &Path) -> Result<()> {
    use std::path::Component;
    // The lexical refusal comes first: the walk below stops at the first
    // missing component, so a `..` after a missing prefix would otherwise
    // never be seen and the OS would apply it later.
    if root
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(Error::rejected(format!(
            "{} is not a plain absolute home — refusing a CADENCE_HOME with `..` or a path \
             prefix; name the installation's own absolute path",
            root.display()
        )));
    }
    let mut prefix = PathBuf::new();
    for component in root.components() {
        match component {
            Component::RootDir => prefix.push("/"),
            Component::Normal(part) => prefix.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) => unreachable!("checked above"),
        }
        match std::fs::symlink_metadata(&prefix) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::rejected(format!(
                    "{} is reached through the symlink {} — refusing to use it as CADENCE_HOME; \
                     name the real installation directory",
                    root.display(),
                    prefix.display()
                )))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => break,
        }
    }
    Ok(())
}

/// Refuse a path under the consolidated home that is reached through a
/// symlink, at any level from the home root down to the leaf.
///
/// The home is created by this installation, so nothing legitimate in it
/// predates it: a symlinked `~/.cadence`, `~/.cadence/config` or
/// `~/.cadence/config/remote-auth` is planted evidence that would send
/// credential reads and writes somewhere else. Same-uid planting is the
/// threat the ticket names, and a leaf-only check misses every ancestor.
pub(super) fn refuse_symlinked_under(root: &Path, path: &Path) -> Result<()> {
    let Ok(relative) = path.strip_prefix(root) else {
        return Ok(());
    };
    // A `..` is resolved by the OS *after* it follows symlinks, so a walk
    // that folds it lexically could check a path the write never takes.
    // Generated paths never contain one; refuse instead of guessing.
    if relative
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(Error::rejected(format!(
            "{} contains a `..` component — refusing to resolve a consolidated path \
             through it; name the installation with an absolute CADENCE_HOME instead",
            path.display()
        )));
    }
    let mut current = root.to_path_buf();
    loop {
        if std::fs::symlink_metadata(&current).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(Error::rejected(format!(
                "{} is reached through a symlink — refusing to read or write the \
                 consolidated home through it; remove the link or set CADENCE_HOME to the \
                 installation to use",
                current.display()
            )));
        }
        if current == path {
            return Ok(());
        }
        match path.strip_prefix(&current).ok().and_then(|rest| {
            rest.components()
                .next()
                .map(|c| current.join(c.as_os_str()))
        }) {
            Some(next) if next != current => current = next,
            _ => return Ok(()),
        }
    }
}

/// Refuse an effect on the invoking user's real consolidated home in a
/// test process. [`create_home_dir`] already answers this for its own
/// callers; a writer that creates with `create_dir_all` directly (setup's
/// state and tracker initialization) calls this before its first effect.
#[cfg(not(any(test, feature = "test-seam")))]
pub fn refuse_real_home(_dir: &Path) -> Result<()> {
    Ok(())
}

/// [`refuse_real_home`] for builds that can run tests.
#[cfg(any(test, feature = "test-seam"))]
pub fn refuse_real_home(dir: &Path) -> Result<()> {
    guard_home(dir.to_path_buf()).map(|_| ())
}

/// Create a directory that belongs to the consolidated home — the config
/// root, a credential directory, the state or the tracker dir. When it
/// sits inside the resolved home, the directory and every missing
/// ancestor below the home root (the home root itself included) are
/// created 0700, so a first run's home is private from the first write;
/// elsewhere this is the plain `create_dir_all` the caller already had.
///
/// An existing directory is never re-created or widened. The one mode a
/// caller may find tightened is a first run's own home root, which stays
/// 0700 whoever created it first (a setup run, the daemon, a login): it
/// is that installation's private home, not an existing installation's
/// directory, and the legacy branch is never touched.
pub fn create_home_dir(dir: &Path) -> Result<()> {
    // An unresolvable layout (no HOME, an unreadable probe) must not
    // block creating a dir the caller already named: that is the plain
    // creation this helper replaced.
    let Ok(layout) = layout() else {
        std::fs::create_dir_all(dir)?;
        return Ok(());
    };
    if !dir.starts_with(&layout.root) {
        std::fs::create_dir_all(dir)?;
        return Ok(());
    }
    // CAD-1227: a test process must never create in the invoking user's
    // real `~/.cadence`. The guard sits on creation, not on resolution:
    // resolving the branch is a read-only question (a daemon fixture
    // asks it for its vault), while creating the home is the effect the
    // guard exists to refuse.
    guard_home(layout.root.clone())?;
    create_private_tree(&layout.root, dir)?;
    if layout.source == Source::Fresh {
        tighten_private(&layout.root)?;
    }
    Ok(())
}

/// Narrow `dir` to 0700 when it is a real directory with group or other
/// bits set. A symlink or a file is left alone.
fn tighten_private(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.mode() & 0o077 == 0 {
        return Ok(());
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Create `dir` and every missing directory between `root` and it with
/// mode 0700. `root` must contain `dir`; nothing above `root` is created
/// or changed, and an existing directory keeps its mode.
pub fn create_private_tree(root: &Path, dir: &Path) -> Result<()> {
    if !dir.starts_with(root) {
        return Err(Error::rejected(format!(
            "{} is not inside {} — refusing to create it as a private tree",
            dir.display(),
            root.display()
        )));
    }
    if dir.exists() {
        return Ok(());
    }
    use std::os::unix::fs::DirBuilderExt;
    let mut missing = Vec::new();
    let mut cur = Some(dir);
    while let Some(path) = cur {
        if path.exists() {
            break;
        }
        missing.push(path);
        if path == root {
            break;
        }
        cur = path.parent();
    }
    for path in missing.iter().rev() {
        match std::fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Resolve an instance's workspace from its captured daemon environment,
/// never from a request or a later process-global layout switch.
///
/// `sandbox_root` is the checked root of the `dev` sandbox this instance
/// is pinned to (`sandbox::active_home`), if any. A sandbox's children do
/// not carry `CADENCE_HOME` (it is read as production evidence), so
/// without this input the workspace would fall back to the host's own
/// `$HOME/.cadence/workspace` and a sandbox daemon would bind the
/// production upload workspace. The sandbox is its own home.
pub fn workspace_for_instance(
    tracker: &Path,
    sandbox_root: Option<&Path>,
    cadence_home: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf> {
    let (home_root, vault) = match (sandbox_root, cadence_home) {
        (Some(root), _) => {
            let root = checked_root(root)?;
            (root.clone(), root.join("vault"))
        }
        (None, Some(raw)) => {
            let root = PathBuf::from(raw);
            if !root.is_absolute() {
                return Err(Error::rejected("CADENCE_HOME must be an absolute path"));
            }
            let root = checked_root(&guard_home(root)?)?;
            (root.clone(), root.join("vault"))
        }
        (None, None) => {
            let user_home = match home {
                Some(raw) => {
                    let path = PathBuf::from(raw);
                    if !path.is_absolute() {
                        return Err(Error::rejected("HOME must be an absolute path"));
                    }
                    path
                }
                None => user_home()?,
            };
            let root = checked_root(&user_home.join(".cadence"))?;
            let tracker = checked_root(tracker)?;
            (root, tracker.join("wiki"))
        }
    };
    let workspace = checked_root(&home_root.join("workspace"))?;
    let tracker = checked_root(tracker)?;
    let vault = checked_root(&vault)?;
    for protected in [&tracker, &vault] {
        if workspace.starts_with(protected) || protected.starts_with(&workspace) {
            return Err(Error::rejected(
                "operational upload workspace overlaps tracker or knowledge vault",
            ));
        }
    }
    Ok(workspace)
}

/// The upload workspace a daemon binds, from its captured provider
/// environment — the one place the daemon's resolution lives, so a check
/// can reach the real wiring. A sandbox profile in that environment pins
/// the sandbox root; a profile that resolves to no valid root is refused
/// rather than falling back to the host's `$HOME/.cadence/workspace`.
pub(crate) fn workspace_for_provider_env(
    tracker: &Path,
    env: &crate::adapter::ProviderEnv,
) -> Result<PathBuf> {
    let sandbox_root = crate::sandbox::pinned_home(|name| env.var(name))?;
    workspace_for_instance(
        tracker,
        sandbox_root.as_deref(),
        env.var("CADENCE_HOME").as_deref(),
        env.var("HOME").as_deref(),
    )
}

/// Refuse an operational workspace that overlaps any read or write root
/// granted to either supported Master provider. The policies are built by
/// the adapters themselves so this check follows their effective authority.
pub fn validate_workspace_isolated(
    workspace: &Path,
    state_dir: &Path,
    env: &crate::adapter::ProviderEnv,
) -> Result<()> {
    let workspace = checked_root(workspace)?;
    let policies = [
        crate::adapter::claude::master_confinement(env, state_dir).1,
        crate::adapter::pi::pi_master_confinement(env, state_dir).1,
    ];
    for policy in policies {
        validate_master_policy(&workspace, policy)?;
    }
    Ok(())
}

/// When a daemon-bound provider is launched, validate and freeze the exact
/// policy that will be encoded into its `cadence confine` argv.
pub(crate) fn validate_policy_for_env(
    policy: crate::confine::Policy,
    env: &crate::adapter::ProviderEnv,
) -> Result<crate::confine::Policy> {
    match env.workspace() {
        Some(workspace) => validate_master_policy(workspace, policy),
        None => Ok(policy),
    }
}

/// Normalize every granted path to the target Landlock will follow, and
/// reject bidirectional overlap with the immutable operational workspace.
fn validate_master_policy(
    workspace: &Path,
    mut policy: crate::confine::Policy,
) -> Result<crate::confine::Policy> {
    let workspace = checked_root(workspace)?;
    for (kind, paths) in [("read", &mut policy.read), ("write", &mut policy.write)] {
        for path in paths.iter_mut() {
            let authority = if process_relative_proc_authority(path).map_err(|reason| {
                Error::rejected(format!(
                    "cannot establish Master {kind} authority for workspace isolation at {}: {reason}",
                    path.display()
                ))
            })? {
                // `/proc/self` and `/proc/thread-self` are interpreted by
                // the confined child, not by this daemon. Keep their spelling
                // intact; canonicalizing here would grant the daemon's PID.
                if workspace.starts_with("/proc") {
                    return Err(Error::rejected(
                        "operational upload workspace cannot be isolated from process-relative /proc authority",
                    ));
                }
                path.clone()
            } else {
                normalize_authority(path).map_err(|reason| {
                    Error::rejected(format!(
                        "cannot establish Master {kind} authority for workspace isolation at {}: {reason}",
                        path.display()
                    ))
                })?
            };
            if workspace.starts_with(&authority) || authority.starts_with(&workspace) {
                return Err(Error::rejected(format!(
                    "operational upload workspace overlaps effective Master {kind} filesystem grant"
                )));
            }
            *path = authority;
        }
    }
    Ok(policy)
}

/// Detect the kernel-relative process aliases whose meaning changes from
/// this daemon to the Master child. Their spelling must reach Landlock intact.
fn process_relative_proc_authority(path: &Path) -> std::result::Result<bool, &'static str> {
    use std::path::Component;
    if !path.is_absolute() {
        return Ok(false);
    }
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::RootDir))
        || !matches!(components.next(), Some(Component::Normal(proc)) if proc == "proc")
    {
        return Ok(false);
    }
    if !matches!(components.next(), Some(Component::Normal(alias)) if alias == "self" || alias == "thread-self")
    {
        return Ok(false);
    }
    if components.any(|component| matches!(component, Component::ParentDir)) {
        return Err("process-relative /proc authority contains parent traversal");
    }
    Ok(true)
}

/// Resolve an authority path as Landlock will: follow existing symlink
/// aliases, then retain a normalized missing suffix below its real ancestor.
/// Unlike workspace roots, grants may name files as well as directories.
fn normalize_authority(path: &Path) -> std::result::Result<PathBuf, &'static str> {
    use std::{collections::VecDeque, path::Component};

    fn enqueue(
        path: &Path,
        from_link: bool,
        pending: &mut VecDeque<(PathBuf, bool)>,
    ) -> std::result::Result<(), &'static str> {
        for component in path.components() {
            match component {
                Component::RootDir => pending.push_back((PathBuf::from("/"), from_link)),
                Component::CurDir => pending.push_back((PathBuf::from("."), from_link)),
                Component::ParentDir => pending.push_back((PathBuf::from(".."), from_link)),
                Component::Normal(part) => pending.push_back((PathBuf::from(part), from_link)),
                Component::Prefix(_) => return Err("path has an unsupported prefix"),
            }
        }
        Ok(())
    }

    fn is_process_relative_proc(path: &Path) -> bool {
        path == Path::new("/proc/self")
            || path.starts_with("/proc/self/")
            || path == Path::new("/proc/thread-self")
            || path.starts_with("/proc/thread-self/")
    }

    if !path.is_absolute() {
        return Err("path is not absolute");
    }
    let mut pending = VecDeque::new();
    enqueue(path, false, &mut pending)?;
    let mut resolved = PathBuf::from("/");
    let mut missing = false;
    let mut symlink_hops = 0usize;

    while let Some((component, from_link)) = pending.pop_front() {
        if component == Path::new("/") {
            if from_link {
                resolved = PathBuf::from("/");
            }
            continue;
        }
        if component == Path::new(".") {
            continue;
        }
        if component == Path::new("..") {
            if missing {
                return Err("parent traversal after a missing path component is ambiguous");
            }
            if resolved != Path::new("/") {
                match std::fs::metadata(&resolved) {
                    Ok(metadata) if metadata.is_dir() => {}
                    _ => return Err("parent traversal does not follow a directory"),
                }
                resolved.pop();
            }
            continue;
        }

        resolved.push(&component);
        if is_process_relative_proc(&resolved) {
            return Err("ordinary alias resolves through process-relative /proc authority");
        }
        if missing {
            if from_link {
                return Err("symlink target is dangling");
            }
            continue;
        }
        match std::fs::symlink_metadata(&resolved) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                symlink_hops += 1;
                if symlink_hops > 40 {
                    return Err("symlink resolution exceeded 40 hops");
                }
                let target = std::fs::read_link(&resolved).map_err(|_| "symlink cannot be read")?;
                resolved.pop();
                if target.is_absolute() {
                    resolved = PathBuf::from("/");
                }
                let mut target_components = VecDeque::new();
                enqueue(&target, true, &mut target_components)?;
                while let Some(item) = target_components.pop_back() {
                    pending.push_front(item);
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if from_link {
                    return Err("symlink target is dangling");
                }
                missing = true;
            }
            Err(_) => return Err("path cannot be inspected"),
        }
    }
    Ok(resolved)
}

/// Resolve an authoritative storage root without adopting `..`, relative
/// paths, or symlink aliases. Missing suffixes are retained lexically so a
/// root can be validated before first use.
fn checked_root(path: &Path) -> Result<PathBuf> {
    use std::path::Component;
    if !path.is_absolute() {
        return Err(Error::rejected("storage root must be absolute"));
    }
    let mut checked = PathBuf::from("/");
    let mut missing = false;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) if !missing => {
                checked.push(part);
                match std::fs::symlink_metadata(&checked) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(Error::rejected(format!(
                            "storage root {} contains a symlink",
                            path.display()
                        )));
                    }
                    Ok(metadata) if !metadata.is_dir() => {
                        return Err(Error::rejected(format!(
                            "storage root {} contains a non-directory component",
                            path.display()
                        )));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing = true,
                    Err(error) => {
                        return Err(Error::rejected(format!(
                            "cannot validate storage root {}: {error}",
                            path.display()
                        )));
                    }
                }
            }
            Component::Normal(part) => checked.push(part),
            _ => return Err(Error::rejected("storage root is not normalized")),
        }
    }
    Ok(checked)
}
