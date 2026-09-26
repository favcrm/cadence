//! The `CADENCE_HOME` resolver — the single source of every cadence
//! path (CAD-392 expand step; decision D5, 2026-09-25).
//!
//! One home layout: `<home>/{state,tracker,vault,repos}` — default
//! `~/.cadence` per user, `/var/lib/cadence` for a system install.
//! Resolution order:
//!
//! 1. `$CADENCE_HOME` — must be absolute; the only switch into the
//!    new layout. An operator sets it at daemon launch.
//! 2. The legacy layout — always, otherwise: `tracker_dir` =
//!    `$CADENCE_PM_DIR` or `~/pm`, `state_dir` = `$CADENCE_STATE_DIR`
//!    or `$XDG_STATE_HOME/cadence` or `~/.local/state/cadence`,
//!    `vault_dir` = `<tracker>/wiki` (CAD-579's wiki lives there
//!    until the migration).
//!
//! The `~/.cadence/LAYOUT` marker is deliberately not consulted:
//! an unconfined agent shares the daemon's uid and could plant the
//! marker to redirect tracker/state/vault on the next restart — an
//! owner/mode check does not help against a same-uid writer. It is
//! read only to be reported by `cadence doctor --host`; the
//! operator-gated activation switch belongs to CAD-392's migrate
//! step.
//!
//! Per-directory overrides keep today's precedence in every branch:
//! `$CADENCE_PM_DIR` and `$CADENCE_STATE_DIR` still beat the resolved
//! default for their own directory, and the `--state-dir` flag still
//! bypasses this module at the CLI layer. Nothing here moves data —
//! the expand step only resolves.

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
    /// No `$CADENCE_HOME` — today's split layout (`~/pm`, legacy
    /// state dir). A `LAYOUT` marker never selects the new layout.
    Legacy,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "CADENCE_HOME",
            Self::Legacy => "legacy",
        }
    }
}

/// The resolved layout: the branch taken plus the home root. Under
/// [`Source::Legacy`] `root` is the would-be `~/.cadence` — nothing
/// reads it until the migration, but `repos_dir` and friends still
/// resolve deterministically.
#[derive(Clone, Debug)]
pub struct Layout {
    pub source: Source,
    pub root: PathBuf,
    /// `root` carries a `LAYOUT` marker containing `1` — reporting
    /// only, for `cadence doctor --host`. A marker never activates
    /// the new layout; only `$CADENCE_HOME` does.
    pub marker_present: bool,
}

/// Which branch the resolver takes for this process, with the home
/// root. Refuses a non-absolute `$CADENCE_HOME` — a relative root
/// would follow the cwd.
pub fn layout() -> Result<Layout> {
    if let Some(root) = std::env::var_os("CADENCE_HOME").map(PathBuf::from) {
        if !root.is_absolute() {
            return Err(Error::rejected(format!(
                "CADENCE_HOME must be an absolute path, got {}",
                root.display()
            )));
        }
        return Ok(Layout {
            source: Source::Env,
            marker_present: layout_marker_declared(&root),
            root,
        });
    }
    let root = default_home_root()?;
    Ok(Layout {
        source: Source::Legacy,
        marker_present: layout_marker_declared(&root),
        root,
    })
}

/// The resolved home root — `$CADENCE_HOME`, else `~/.cadence`
/// whether or not the layout marker is present.
pub fn home() -> Result<PathBuf> {
    Ok(layout()?.root)
}

/// Tracker (pm board) directory: `$CADENCE_PM_DIR`, else
/// `<home>/tracker` under the new layout, else `~/pm`.
pub fn tracker_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CADENCE_PM_DIR") {
        return Ok(PathBuf::from(dir));
    }
    tracker_default()
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
        return Ok(PathBuf::from(dir));
    }
    state_default()
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

/// The knowledge vault root: `<home>/vault` under the new layout;
/// `<tracker_dir>/wiki` under the legacy one (CAD-579's wiki lives
/// there until the CAD-392 migration moves it).
pub fn vault_dir() -> Result<PathBuf> {
    match layout()? {
        Layout {
            source: Source::Legacy,
            ..
        } => Ok(tracker_dir()?.join("wiki")),
        l => Ok(l.root.join("vault")),
    }
}

/// Managed repo checkouts: `<home>/repos` — under the legacy layout
/// the would-be `~/.cadence/repos` (nothing reads it until the
/// migration creates the layout).
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
