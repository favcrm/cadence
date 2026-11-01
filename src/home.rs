//! The `CADENCE_HOME` resolver — the single source of every cadence
//! path (CAD-392 expand step; decision D5, 2026-09-25).
//!
//! One home layout: `<home>/{state,tracker,vault,repos,workspace,config}`
//! — `~/.cadence` per user, `/var/lib/cadence` for a system install
//! (CAD-1922; decision D5, 2026-09-25). Resolution order:
//!
//! 1. `$CADENCE_HOME` — must be absolute. An operator or installer sets
//!    it, and a sandbox pins it to its own root.
//! 2. A first run ([`Source::Fresh`]): nothing of an existing
//!    installation exists under the legacy paths, so `~/.cadence` is the
//!    default home — including inside a CI job's or container's own
//!    isolated `$HOME`.
//! 3. The legacy layout — always, otherwise: `tracker_dir` =
//!    `$CADENCE_PM_DIR` or `~/pm`, `state_dir` = `$CADENCE_STATE_DIR`
//!    or `$XDG_STATE_HOME/cadence` or `~/.local/state/cadence`,
//!    `vault_dir` = `<tracker>/wiki`, hosted config/credentials in
//!    `$XDG_CONFIG_HOME/cadence` (CAD-579's wiki lives there until a
//!    migration).
//!
//! **Nothing here moves, creates or deletes data, and no existing
//! installation is migrated.** An installation in the legacy layout
//! keeps its actual tracker, state, vault, configuration and
//! credentials: the presence of any one of them — or an exported
//! per-directory override — keeps every path on the legacy branch, so
//! resolution can never switch a live install out from under its data.
//! The new layout is chosen only when there is nothing to preserve.
//!
//! The `~/.cadence/LAYOUT` marker is deliberately not consulted:
//! an unconfined agent shares the daemon's uid and could plant the
//! marker to redirect tracker/state/vault on the next restart — an
//! owner/mode check does not help against a same-uid writer. It is
//! read only to be reported by `cadence doctor --host`; an
//! operator-gated migration switch belongs to CAD-392's migrate step,
//! which this module does not implement.
//!
//! Per-directory overrides keep today's precedence in every branch:
//! `$CADENCE_PM_DIR` and `$CADENCE_STATE_DIR` still beat the resolved
//! default for their own directory, and the `--state-dir` and
//! `--auth-dir` flags still bypass this module at the CLI layer. An
//! override overrides only its own directory — it never chooses the
//! branch, because `cadence switch local` and a sandbox export it into
//! their own process and a layout flip mid-command would move the vault
//! and the hosted config. Nothing here moves, copies or deletes data, and
//! no installation is migrated: the module only resolves, and a
//! migration stays a separate operator-approved, backed-up operation.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Marker file inside a home root declaring the new layout. The
/// migration writes it; the content is the layout version.
pub const LAYOUT_MARKER: &str = "LAYOUT";

/// The only layout version this build understands.
pub const LAYOUT_VERSION: &str = "1";

/// Which branch resolved the home.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `$CADENCE_HOME` — an explicit new-layout root.
    Env,
    /// A `dev` sandbox this process is pinned to (CAD-1922). Its root is
    /// the sandbox: the same layout a `CADENCE_HOME` would name, without
    /// exporting one, so the overlap guard can read `CADENCE_HOME` as
    /// production evidence.
    Sandbox,
    /// A first run: no legacy installation exists, so `~/.cadence` is
    /// the default home (CAD-1922).
    Fresh,
    /// An existing installation: today's split layout (`~/pm`, legacy
    /// state dir, `$XDG_CONFIG_HOME/cadence`). A `LAYOUT` marker never
    /// selects the new layout.
    Legacy,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "CADENCE_HOME",
            Self::Fresh => "fresh",
            Self::Sandbox => "sandbox",
            Self::Legacy => "legacy",
        }
    }
}

/// The resolved layout: the branch taken plus the home root. Under
/// [`Source::Legacy`] `root` is the would-be `~/.cadence` — nothing
/// reads it until a migration, but `repos_dir` and friends still
/// resolve deterministically.
#[derive(Clone, Debug)]
pub struct Layout {
    pub source: Source,
    pub root: PathBuf,
    /// `root` carries a `LAYOUT` marker containing `1` — reporting
    /// only, for `cadence doctor --host`. A marker never activates
    /// the new layout; only `$CADENCE_HOME` or a first run does.
    pub marker_present: bool,
}

/// Which branch the resolver takes for this process, with the home
/// root. Refuses a non-absolute `$CADENCE_HOME` — a relative root
/// would follow the cwd.
pub fn layout() -> Result<Layout> {
    // A sandbox is its own home. The profile names it and the marker
    // proves the root exists, so no `CADENCE_HOME` has to be exported for
    // the sandbox's children to keep the caller's vault, repos, workspace
    // and credentials out — which leaves that variable free to mean
    // production evidence in the overlap guard.
    if let Some(root) = crate::sandbox::active_root() {
        return Ok(Layout {
            source: Source::Sandbox,
            marker_present: layout_marker_declared(&root),
            root,
        });
    }

    if let Some(root) = std::env::var_os("CADENCE_HOME").map(PathBuf::from) {
        if !root.is_absolute() {
            return Err(Error::rejected(format!(
                "CADENCE_HOME must be an absolute path, got {}",
                root.display()
            )));
        }
        refuse_symlinked_ancestry(&root)?;
        let root = guard_home(root)?;
        return Ok(Layout {
            source: Source::Env,
            marker_present: layout_marker_declared(&root),
            root,
        });
    }
    layout_without_env()
}

/// [`layout`] with `$CADENCE_HOME` unset — the production default this
/// host would use, and what a sandbox compares its own roots against.
fn layout_without_env() -> Result<Layout> {
    let root = default_home_root()?;
    let marker_present = layout_marker_declared(&root);
    match installation(&root)? {
        Installation::Legacy => Ok(Layout {
            source: Source::Legacy,
            marker_present,
            root,
        }),
        Installation::Consolidated | Installation::FirstRun => Ok(Layout {
            source: Source::Fresh,
            marker_present,
            root,
        }),
        Installation::Ambiguous => Err(Error::rejected(format!(
            "two Cadence installations exist: {} holds state/, tracker/ or config/ \
             and the legacy paths ({} or {} or {}) exist too — refusing to pick \
             one. Remove the stray home, or set CADENCE_HOME to name the \
             installation to use",
            root.display(),
            legacy_tracker_dir()?.display(),
            legacy_state_dir()?.display(),
            legacy_config_dir()?.display()
        ))),
    }
}

/// The resolved home root — `$CADENCE_HOME`, else `~/.cadence`
/// whether or not the layout marker is present.
pub fn home() -> Result<PathBuf> {
    Ok(layout()?.root)
}

/// [`home`] with `$CADENCE_HOME` unset — the production home a sandbox
/// must never reach, whatever the caller's own environment says.
pub fn home_default() -> Result<PathBuf> {
    Ok(layout_without_env()?.root)
}

/// Which installation this machine already has, from filesystem evidence
/// alone (CAD-1922).
///
/// The decision never looks at a per-directory override: `cadence switch
/// local` and `sandbox::adopt` export `CADENCE_STATE_DIR`/`CADENCE_PM_DIR`
/// into their own process, so letting an export pick the branch would flip
/// the layout of the rest of that command (vault, config) and let it
/// create a phantom tracker. An override only overrides its own
/// directory.
///
/// The branch is sticky and ambiguity fails closed: once the consolidated
/// home holds `state/`, `tracker/` or `config/`, a legacy path that
/// appears later — a stray `~/pm`, an old binding — can neither flip the
/// installation to the legacy layout nor be silently preferred. Both at
/// once is refused, naming every root, so nothing is guessed and no data
/// is moved. A probe that cannot be answered (a permission or I/O error, a
/// dangling symlink) counts as present. Release and test-seam builds
/// resolve identically.
fn installation(root: &Path) -> Result<Installation> {
    let legacy_paths = [
        legacy_tracker_dir()?,
        legacy_state_dir()?,
        local_state_dir()?,
        legacy_config_dir()?,
    ];
    // A per-directory override that names one of the legacy roots is the
    // user's explicit choice of that root. Creating it must not then make
    // the installation ambiguous on the next run, so naming it counts as
    // legacy evidence here — the one case where an export decides the
    // branch, because the effect would otherwise decide it silently.
    let exported_legacy = ["CADENCE_STATE_DIR", "CADENCE_PM_DIR"].iter().any(|name| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .is_some_and(|path| legacy_paths.iter().any(|legacy| same_place(&path, legacy)))
    });
    let legacy = exported_legacy || legacy_paths.iter().any(|path| legacy_present(path));
    let consolidated = [
        root.join("state"),
        root.join("tracker"),
        root.join("config"),
    ]
    .iter()
    .any(|path| legacy_present(path));
    Ok(match (legacy, consolidated) {
        (false, false) => Installation::FirstRun,
        (false, true) => Installation::Consolidated,
        (true, false) => Installation::Legacy,
        (true, true) => Installation::Ambiguous,
    })
}

/// What [`installation`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Installation {
    /// Nothing anywhere: a genuine first run.
    FirstRun,
    /// The consolidated home already holds this machine's installation.
    Consolidated,
    /// The legacy split layout holds it.
    Legacy,
    /// Both hold one — never guessed, always refused.
    Ambiguous,
}

/// Whether `path` exists as anything at all — a directory, a file or a
/// dangling symlink. A probe that fails for any other reason (permission,
/// I/O) counts as present: resolution then stays on the legacy branch
/// rather than switching an installation it could not inspect.
fn legacy_present(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Tracker (pm board) directory: `$CADENCE_PM_DIR`, else
/// `<home>/tracker` under the new layout, else `~/pm`.
pub fn tracker_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CADENCE_PM_DIR") {
        return test_guard(PathBuf::from(dir), ".", "pm");
    }
    test_guard(tracker_default()?, ".", "pm")
}

/// `tracker_dir` with `CADENCE_PM_DIR` unset — the production
/// tracker a sandbox must never reach.
pub fn tracker_default() -> Result<PathBuf> {
    match layout()? {
        Layout {
            source: Source::Legacy,
            ..
        } => legacy_tracker_dir(),
        l => Ok(l.root.join("tracker")),
    }
}

/// State directory: `$CADENCE_STATE_DIR`, else `<home>/state` under
/// the new layout, else the legacy default.
pub fn state_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CADENCE_STATE_DIR") {
        return test_guard(PathBuf::from(dir), ".local/state", "cadence");
    }
    test_guard(state_default()?, ".local/state", "cadence")
}

/// `state_dir` with `CADENCE_STATE_DIR` unset — the production
/// state dir a sandbox must never reach.
pub fn state_default() -> Result<PathBuf> {
    match layout()? {
        Layout {
            source: Source::Legacy,
            ..
        } => legacy_state_dir(),
        l => Ok(l.root.join("state")),
    }
}

/// The knowledge vault root. The vault follows the tracker it belongs
/// to: a tracker inside the resolved home keeps `<home>/vault`, and a
/// tracker outside it keeps `<tracker>/wiki` (CAD-579's wiki lives
/// beside its tracker). An explicit `CADENCE_HOME` keeps `<home>/vault`
/// for every tracker, as it did before.
///
/// The tracker's location — not the process branch — decides this, so a
/// per-directory override (`cadence switch local`, `sandbox::adopt`)
/// can never move a vault or make one appear at a phantom path.
pub fn vault_dir() -> Result<PathBuf> {
    vault_for(&tracker_dir()?)
}

/// The vault a *served* tracker uses — the instance-bound form of
/// [`vault_dir`]: `<home>/vault` when `tracker` sits inside the resolved
/// home (or `$CADENCE_HOME` is set); `tracker`'s `wiki/` otherwise.
/// [`vault_dir`] resolves the env tracker, but a daemon's tracker is
/// instance-bound (its own `CADENCE_PM_DIR` lives in `provider_env`,
/// which process-env resolution cannot see) — under a test fixture the
/// two disagree, and the wiki must follow the tracker actually being
/// served.
pub fn vault_for(tracker: &Path) -> Result<PathBuf> {
    let layout = layout()?;
    if matches!(layout.source, Source::Env | Source::Sandbox) {
        return Ok(layout.root.join("vault"));
    }
    // Only the consolidated branch's *own* tracker (`<home>/tracker`) uses
    // the home vault. A `CADENCE_PM_DIR` that merely happens to sit under
    // the home — a legacy install pointed at `~/.cadence/old-pm`, say — is
    // not that tracker, and its wiki must stay beside it: reading the
    // branch would move an existing installation's knowledge root without
    // the migration that CAD-392 still owns.
    if same_place(tracker, &layout.root.join("tracker")) {
        return Ok(layout.root.join("vault"));
    }
    Ok(tracker.join("wiki"))
}

/// Whether two paths name the same place. The comparison canonicalises
/// first — so `~/.cadence/alias/../tracker` with `alias` present is the
/// branch's own tracker, exactly as the OS resolves it — and falls back
/// to folding `.`/`..` for a path that does not exist yet.
fn same_place(a: &Path, b: &Path) -> bool {
    comparable(a) == comparable(b)
}

fn comparable(p: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(p) {
        return real;
    }
    let mut out = PathBuf::new();
    for component in p.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Operational upload workspace: `<resolved-home>/workspace`. This is
/// independent of tracker/state overrides and does not activate any part
/// of the broader home-layout migration.
pub fn workspace_dir() -> Result<PathBuf> {
    Ok(layout()?.root.join("workspace"))
}

/// Hosted config and credentials root. A first run — and an installation
/// already living in the consolidated home — keeps them in `<home>/config`.
/// A legacy installation keeps `$XDG_CONFIG_HOME/cadence` (else
/// `~/.config/cadence`), where its credentials already are; so does an
/// installation given an explicit `CADENCE_HOME` whose `XDG_CONFIG_HOME` is
/// exported, or whose legacy root already holds something.
///
/// The decision follows the branch, not the environment: exporting
/// `XDG_CONFIG_HOME` does not move a fresh installation's credentials into
/// the legacy root, because the next run would then see both layouts and
/// refuse the installation as ambiguous. A probe that cannot be answered
/// counts as present, so credentials the resolver could not inspect are
/// never abandoned. Under the consolidated home the path is refused when
/// any level is a symlink, so a planted link cannot redirect credentials.
///
/// The directory is private: [`create_home_dir`] makes it and the home
/// root above it 0700 on first use, and the credential files inside stay
/// 0600. `--auth-dir` still bypasses this resolver at the CLI layer.
pub fn config_dir() -> Result<PathBuf> {
    let layout = layout()?;
    config_root(&layout)
}

/// [`config_dir`] with `$CADENCE_HOME` unset — the production config a
/// sandbox must never reach.
pub fn config_default() -> Result<PathBuf> {
    let layout = layout_without_env()?;
    config_root(&layout)
}

/// The config root for a resolved layout. One branch, one store:
///
/// - a first run — and an installation already living in the consolidated
///   home — keeps everything, credentials included, under that home.
///   Honouring an exported `XDG_CONFIG_HOME` here would write the first
///   credential into the legacy root, and the next run would then find
///   legacy *and* consolidated evidence and refuse the installation as
///   ambiguous;
/// - a legacy installation keeps `$XDG_CONFIG_HOME/cadence`
///   (`~/.config/cadence`), where its credentials already are;
/// - an explicit `CADENCE_HOME` keeps an exported `XDG_CONFIG_HOME` and an
///   existing legacy root, which name where credentials already live, and
///   otherwise uses `<home>/config`.
///
/// A config root under the consolidated home is then checked for
/// symlinks: a planted link would redirect credential reads and writes,
/// so it is refused rather than followed.
fn config_root(layout: &Layout) -> Result<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME").is_some_and(|value| !value.is_empty());
    let legacy = legacy_config_dir()?;
    let chosen = match layout.source {
        // No `CADENCE_HOME` and no legacy evidence: a first run or an
        // installation already living in the consolidated home. Both keep
        // their config inside it; a legacy config root present at the same
        // time would have made the layout `Ambiguous` and been refused
        // before this point. A sandbox is its own install the same way.
        Source::Fresh | Source::Sandbox => layout.root.join("config"),
        Source::Legacy => legacy,
        Source::Env => {
            if xdg || legacy_present(&legacy) {
                legacy
            } else {
                layout.root.join("config")
            }
        }
    };
    if chosen.starts_with(&layout.root) {
        refuse_symlinked_under(&layout.root, &chosen)?;
    }
    Ok(chosen)
}

/// Refuse a root whose existing ancestry contains a symlink or a `..`.
/// An explicit `CADENCE_HOME` is authoritative for credentials, so a
/// planted link anywhere above it would redirect every write even when the
/// leaf itself is not a link. Nothing below the first missing component is
/// checked; the caller then creates it under the refusal rules above.
fn refuse_symlinked_ancestry(root: &Path) -> Result<()> {
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
fn refuse_symlinked_under(root: &Path, path: &Path) -> Result<()> {
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
pub fn workspace_for_instance(
    tracker: &Path,
    cadence_home: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf> {
    let (home_root, vault) = match cadence_home {
        Some(raw) => {
            let root = PathBuf::from(raw);
            if !root.is_absolute() {
                return Err(Error::rejected("CADENCE_HOME must be an absolute path"));
            }
            let root = checked_root(&guard_home(root)?)?;
            (root.clone(), root.join("vault"))
        }
        None => {
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

/// Refuse an operational workspace that overlaps any read or write root
/// granted to either supported Master provider. The policies are built by
/// the adapters themselves so this check follows their effective authority.
pub(crate) fn validate_workspace_isolated(
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
pub(crate) fn validate_master_policy(
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

/// Managed repo checkouts: `<home>/repos` — under the legacy layout
/// the same `~/.cadence/repos`, which an existing installation already
/// uses for managed checkouts.
pub fn repos_dir() -> Result<PathBuf> {
    Ok(layout()?.root.join("repos"))
}

/// Content-addressed blob store under a vault root — `<root>/.blobs`,
/// gitignored and refused by tracker lint (CAD-392 layout, CAD-580).
pub fn blobs_dir(root: &Path) -> PathBuf {
    root.join(".blobs")
}

/// `root` carries a `LAYOUT` marker containing `1`. Reporting only —
/// never consulted for layout selection, because a same-uid writer
/// could plant it (rev-300, CAD-584). CAD-392's migrate step owns
/// the operator-gated activation switch.
fn layout_marker_declared(root: &Path) -> bool {
    std::fs::read_to_string(root.join(LAYOUT_MARKER))
        .map(|v| v.trim() == LAYOUT_VERSION)
        .unwrap_or(false)
}

/// `~/.cadence` — requires `$HOME` to be an absolute path, the same
/// discipline the tracker default uses today.
fn default_home_root() -> Result<PathBuf> {
    Ok(user_home()?.join(".cadence"))
}

fn user_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| Error::rejected("HOME is not set to an absolute path"))
}

/// The pre-`CADENCE_HOME` tracker default: `~/pm`.
fn legacy_tracker_dir() -> Result<PathBuf> {
    Ok(user_home()?.join("pm"))
}

/// `~/.local/state/cadence` — where a process without
/// `XDG_STATE_HOME` keeps runtime state, whatever the layout. The
/// sandbox guard compares against it because a daemon started
/// without `XDG_STATE_HOME` lives here even when this shell sets it.
pub fn local_state_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| Error::internal("Cannot locate HOME for state directory"))?;
    Ok(PathBuf::from(home).join(".local/state/cadence"))
}

/// The pre-`CADENCE_HOME` state default: `$XDG_STATE_HOME/cadence`,
/// else `~/.local/state/cadence`.
fn legacy_state_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        return Ok(PathBuf::from(dir).join("cadence"));
    }
    local_state_dir()
}

/// The pre-`CADENCE_HOME` hosted config root — `$XDG_CONFIG_HOME/cadence`,
/// else `~/.config/cadence`: where `orgs.json` and `remote-auth/` live
/// for every installation that predates the consolidated home.
fn legacy_config_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| Error::rejected("Set HOME, XDG_CONFIG_HOME or --auth-dir"))?;
    if !base.is_absolute() {
        return Err(Error::rejected("Credential directory must be absolute"));
    }
    Ok(base.join("cadence"))
}

/// `pw_dir` for the invoking uid. `None` when the passwd database has no
/// entry.
#[cfg(any(test, feature = "test-seam"))]
fn passwd_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: getpwuid returns null or a pointer to a static passwd
    // record whose pw_dir is a NUL-terminated string; copied at once.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*pw).pw_dir);
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
    }
}

/// CAD-1210: a test process must never reach the invoking user's real
/// tracker (`~/pm`) or state dir (`~/.local/state/cadence`). Armed
/// only under `cfg(test)` and the `test-seam` feature (integration
/// targets build the lib without `cfg(test)`); a release build is a
/// no-op. Allowlist-shaped: only those two production roots refuse.
#[cfg(not(any(test, feature = "test-seam")))]
fn test_guard(dir: PathBuf, _rel: &str, _leaf: &str) -> Result<PathBuf> {
    Ok(dir)
}

#[cfg(any(test, feature = "test-seam"))]
fn test_guard(dir: PathBuf, rel: &str, leaf: &str) -> Result<PathBuf> {
    test_guard_in(dir, real_home().as_deref(), rel, leaf)
}

#[cfg(any(test, feature = "test-seam"))]
fn test_guard_in(dir: PathBuf, real: Option<&Path>, rel: &str, leaf: &str) -> Result<PathBuf> {
    match real {
        Some(real) => {
            let mut root = real.to_path_buf();
            if rel != "." {
                root.push(rel);
            }
            let root = root.join(leaf);
            // CAD-1227: the one allowlisted path under a refused root: a
            // queued test job's own isolation tree (a fresh empty state
            // dir the daemon creates per job). Strictly below `run/`.
            if leaf == "cadence" {
                let run = resolve_for_compare(&root.join("test-queue/run"));
                let got = resolve_for_compare(&dir);
                if got != run && got.starts_with(&run) {
                    return Ok(dir);
                }
            }
            refuse_under(dir, &root)
        }
        None => Ok(dir),
    }
}

/// CAD-1227: a test process must not take the invoking user's real
/// `~/.cadence` as its `CADENCE_HOME` either.
fn guard_home(root: PathBuf) -> Result<PathBuf> {
    test_guard(root, ".", ".cadence")
}

/// [`guard_home`] with the real home passed in (pure; for tests).
#[cfg(test)]
pub(crate) fn guard_home_in(root: PathBuf, real: Option<&Path>) -> Result<PathBuf> {
    test_guard_in(root, real, ".", ".cadence")
}

/// The same refusal for a tracker path that did not come through
/// [`tracker_dir`] (a daemon's `provider_env`, the CLI's env read).
pub fn guard_tracker(dir: PathBuf) -> Result<PathBuf> {
    test_guard(dir, ".", "pm")
}

/// [`guard_tracker`] with the real home passed in (pure; for tests).
#[cfg(test)]
pub(crate) fn guard_tracker_in(dir: PathBuf, real: Option<&Path>) -> Result<PathBuf> {
    test_guard_in(dir, real, ".", "pm")
}

/// `p` made comparable: relative paths become absolute against the
/// current directory; the longest existing prefix of the RAW path is
/// canonicalised (so a symlink is followed before any `..` after it is
/// applied, exactly as the OS would); `.` and `..` are folded
/// lexically only in the not-yet-existing tail.
#[cfg(any(test, feature = "test-seam"))]
fn resolve_for_compare(p: &Path) -> PathBuf {
    use std::path::Component;
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(p),
            Err(_) => p.to_path_buf(),
        }
    };
    let comps: Vec<Component> = abs.components().collect();
    for k in (1..=comps.len()).rev() {
        let prefix: PathBuf = comps[..k].iter().collect();
        if let Ok(mut real) = std::fs::canonicalize(&prefix) {
            for c in &comps[k..] {
                match c {
                    Component::ParentDir => {
                        real.pop();
                    }
                    Component::CurDir => {}
                    c => real.push(c.as_os_str()),
                }
            }
            return real;
        }
    }
    abs
}

#[cfg(any(test, feature = "test-seam"))]
fn refuse_under(dir: PathBuf, root: &Path) -> Result<PathBuf> {
    let root = resolve_for_compare(root);
    if resolve_for_compare(&dir).starts_with(&root) {
        return Err(Error::rejected(format!(
            "refusing to resolve {}: a test process must not touch the real {} \
             (CAD-1210); point CADENCE_PM_DIR/CADENCE_STATE_DIR at a temp dir",
            dir.display(),
            root.display()
        )));
    }
    Ok(dir)
}

/// The invoking user's home from the passwd database (never `$HOME`,
/// which tests isolate). `CADENCE_TEST_REAL_HOME` substitutes a fake
/// stand-in; it is read only in these test builds.
#[cfg(any(test, feature = "test-seam"))]
fn real_home() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("CADENCE_TEST_REAL_HOME") {
        return Some(PathBuf::from(h));
    }
    passwd_home()
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    #[test]
    fn production_roots_refuse_and_other_dirs_pass() {
        let fake = PathBuf::from("/tmp/c1210-fakehome");
        let pm = fake.join("pm");
        let st = fake.join(".local/state/cadence");
        let err = refuse_under(pm.join("sub"), &pm).unwrap_err().to_string();
        assert!(err.contains("CAD-1210"), "{err}");
        assert!(refuse_under(st.clone(), &st).is_err());
        assert!(refuse_under(fake.join("pm-other"), &pm).is_ok());
        assert!(refuse_under(fake.join("tmp-state"), &st).is_ok());
    }

    #[test]
    fn symlinked_and_dotdot_paths_to_the_root_refuse() {
        let base = std::env::temp_dir().join(format!("c1210-sl-{}", std::process::id()));
        let pm = base.join("fakehome/pm");
        std::fs::create_dir_all(&pm).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&pm, &link).unwrap();
        // symlink to the root with a tail that does not exist yet
        assert!(refuse_under(link.join("newsub/deeper"), &pm).is_err());
        // `..` through a missing dir, back into the root
        let dd = base.join("nope/../fakehome/pm/x");
        assert!(refuse_under(dd, &pm).is_err());
        // a sibling that merely shares a prefix still passes
        assert!(refuse_under(base.join("fakehome/pm-other/x"), &pm).is_ok());
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Fake real home inside a fresh temp dir; returns (base, fake pm).
    fn fake(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("c1227-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let pm = base.join("fakehome/pm");
        std::fs::create_dir_all(pm.join("sub")).unwrap();
        (base, pm)
    }

    #[test]
    fn symlink_to_subdir_then_dotdot_refuses() {
        let (base, pm) = fake("sd");
        let link = base.join("link");
        std::os::unix::fs::symlink(pm.join("sub"), &link).unwrap();
        // link/.. is pm itself: symlink resolved before `..` folds
        assert!(refuse_under(link.join(".."), &pm).is_err());
        assert!(refuse_under(link.join("../newdir"), &pm).is_err());
        // a symlink out of the root does not
        let other = base.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let out = base.join("out");
        std::os::unix::fs::symlink(&other, &out).unwrap();
        assert!(refuse_under(out.join("x"), &pm).is_ok());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn relative_dotdot_resolves_against_cwd() {
        let (base, pm) = fake("rel");
        // The process cwd is shared; build the relative path from it
        // so no cwd change is needed: `<up to base>/fakehome/pm/x`.
        let cwd = std::env::current_dir().unwrap();
        let mut rel = PathBuf::new();
        for _ in cwd.components().skip(1) {
            rel.push("..");
        }
        let rel_pm = rel.join(pm.strip_prefix("/").unwrap()).join("x");
        assert!(rel_pm.is_relative());
        assert!(refuse_under(rel_pm, &pm).is_err());
        // a relative sibling `pm2` passes
        let sib = base.join("fakehome/pm2");
        std::fs::create_dir_all(&sib).unwrap();
        let rel_sib = rel.join(sib.strip_prefix("/").unwrap());
        assert!(refuse_under(rel_sib, &pm).is_ok());
        // a leading `..` beyond the root is clamped, not dropped into pm
        assert!(refuse_under(PathBuf::from("../../../../../../../../../tmp/zz-none"), &pm).is_ok());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn cadence_home_production_root_refuses() {
        let (base, _pm) = fake("home");
        let real = base.join("fakehome");
        let root = real.join(".cadence");
        std::fs::create_dir_all(&root).unwrap();
        assert!(guard_home_in(root.clone(), Some(&real)).is_err());
        assert!(guard_home_in(root.join("tracker"), Some(&real)).is_err());
        assert!(guard_home_in(base.join("tmp-home"), Some(&real)).is_ok());
        assert!(guard_home_in(real.join(".cadence2"), Some(&real)).is_ok());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn only_the_test_queue_run_tree_is_allowlisted_under_state() {
        let (base, _pm) = fake("tq");
        let real = base.join("fakehome");
        let st = real.join(".local/state/cadence");
        let run = st.join("test-queue/run");
        std::fs::create_dir_all(run.join("tq-1/state")).unwrap();
        let g = |d: PathBuf| test_guard_in(d, Some(&real), ".local/state", "cadence");
        assert!(g(run.join("tq-1/state")).is_ok());
        assert!(g(run.join("tq-2/state")).is_ok());
        // the state root, siblings of run/, run/ itself and `..` escapes refuse
        assert!(g(st.clone()).is_err());
        assert!(g(st.join("test-queue")).is_err());
        assert!(g(st.join("test-queue/jobs")).is_err());
        assert!(g(run.clone()).is_err());
        assert!(g(run.join("tq-1/../../..")).is_err());
        assert!(g(run.join("tq-1/state/../../../queue")).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
