//! Production-overlap and marker ownership guards for dev sandboxes.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};

use super::{base_dir, validate_name, Sandbox, DEV_MARKER, MARKER, PROFILE_PREFIX};

// ---------- production guard ----------

/// Where `path` lands: made absolute, each existing prefix
/// canonicalized (symlinks followed) and a `..` popping the component
/// before it — lexically once the path stops existing, which is where
/// creating it would land too. No spelling of a dir compares
/// differently from the dir itself.
pub(crate) fn resolved(path: &Path) -> PathBuf {
    let path = if path.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::Normal(part) => {
                out.push(part);
                if let Ok(real) = std::fs::canonicalize(&out) {
                    out = real;
                }
            }
            root => out.push(root.as_os_str()),
        }
    }
    out
}

/// The live dirs a sandbox must never overlap, labelled for the refusal.
///
/// The list is evidence about the *host*, not about the candidate: the
/// marker is a hand-writable file and any caller can export any path, so
/// neither may exempt a production dir. Production's own roots are
/// therefore resolved from the host's defaults alone — `$HOME` and
/// `$XDG_*`, plus an exported `$CADENCE_HOME` named as the installation it
/// is (a sandbox's children no longer carry one) — and none is dropped because it happens to sit at or under the candidate
/// root: a marker planted in the production tracker stays a production
/// tracker.
///
/// This inventory exempts nothing a caller exports; the one exemption lives
/// in [`refuse_overlap`] and only the read-only guard
/// ([`refuse_production_readonly`]) takes it, for an export exactly equal to
/// one of the sandbox's own dirs. The deleting verbs never do.
/// `CADENCE_PROFILE`,
/// `CADENCE_HOME`, `CADENCE_STATE_DIR`, `CADENCE_PM_DIR` and
/// `XDG_CONFIG_HOME` are same-uid environment, not identity: a genuine
/// installation may be configured at `<root>/state`, `<root>/pm` or
/// `<root>/config`, so a forged profile or an export equal to one of
/// this sandbox's own paths would let a planted marker in `<root>`
/// authorise deleting that installation. Every export is a production
/// dir. `exported: false` keeps the defaults only.
pub(super) fn production_dirs(
    sb: &Sandbox,
    exported: bool,
) -> Result<Vec<(&'static str, PathBuf)>> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    // CAD-1922: the consolidated home holds `state/`, `tracker/`,
    // `config/`, `repos/`, `vault/` and `workspace/`, so a sandbox inside
    // it would share a live install's roots. It is spelled from `$HOME`
    // like the legacy roots below, not through `home_default()`: that
    // resolver is test-guarded (CAD-1210) and would refuse to name the
    // real `~/.cadence` in a test process that only needs the path.
    let mut dirs: Vec<(&'static str, PathBuf)> = Vec::new();
    // The pre-`CADENCE_HOME` roots, from `$HOME`/`$XDG_*` alone: an
    // existing installation keeps its data here whatever branch the
    // caller's `$CADENCE_HOME` selects, so an export cannot hide it.
    // `home.rs` resolves the same spellings, but its public defaults
    // follow `$CADENCE_HOME`/`$XDG_CONFIG_HOME`; this guard must not.
    // The host's defaults must be establishable: an empty inventory is
    // not proof of isolation. Without `HOME` the production tracker, home
    // and config cannot be named, so a planted marker at `~/pm` would be
    // accepted as disposable — refuse instead of guessing.
    let home = var("HOME").ok_or_else(|| {
        Error::rejected(
            "HOME is not set to an absolute path — refusing to judge a sandbox without \
             the host's production roots",
        )
    })?;
    dirs.push(("the production home", home.join(".cadence")));
    dirs.push(("the production tracker", home.join("pm")));
    dirs.push(("the production config dir", home.join(".config/cadence")));
    // A daemon started without XDG_STATE_HOME lives here even when this
    // shell sets it.
    dirs.push(("the production state dir", crate::home::local_state_dir()?));
    // An explicit `CADENCE_HOME` names a real installation too — a system
    // install, a second profile. Sandbox children no longer carry it (the
    // resolver maps their profile to the sandbox root), so naming it here
    // cannot make a sandbox refuse its own store, and `owner_of` refuses a
    // marker planted in that installation.
    if let Some(root) = var("CADENCE_HOME").filter(|path| path.is_absolute()) {
        dirs.push(("the CADENCE_HOME installation", root.clone()));
        dirs.push(("the CADENCE_HOME state dir", root.join("state")));
        dirs.push(("the CADENCE_HOME tracker", root.join("tracker")));
        dirs.push(("the CADENCE_HOME config dir", root.join("config")));
    }
    if let Some(state_home) = var("XDG_STATE_HOME") {
        dirs.push(("the production state dir", state_home.join("cadence")));
    }
    if let Some(config_home) = var("XDG_CONFIG_HOME") {
        // The config root's own `cadence/` dir is the config *this
        // process* would use. A sandbox's children carry
        // `XDG_CONFIG_HOME=<root>/config`, and `owner_of` (exported:
        // false) must not read that as production. Nothing is exempted
        // here: a deleting verb passes exported: true and sees the
        // export itself, which stays a production dir even at this path.
        if resolved(&config_home) != resolved(&sb.config_dir()) {
            dirs.push(("the production config dir", config_home.join("cadence")));
        }
    }
    if exported {
        // CAD-1187: every export is a claim about a live cadence, with
        // no exemption. `CADENCE_PROFILE` is caller-controlled env, not
        // identity, so neither it nor a path equal to one of this
        // sandbox's own dirs may mark a dir disposable — a custom
        // installation can be configured at `<root>/state` or `<root>/pm`
        // too, and a planted marker would then delete it.
        // `CADENCE_HOME` is inventoried above, from its own value, so it
        // is not repeated here as an export.
        for (name, label) in [
            ("CADENCE_STATE_DIR", "the exported CADENCE_STATE_DIR"),
            ("CADENCE_PM_DIR", "the exported CADENCE_PM_DIR"),
            ("XDG_CONFIG_HOME", "the exported XDG_CONFIG_HOME"),
        ] {
            if let Some(dir) = var(name) {
                dirs.push((label, dir));
            }
        }
    }
    Ok(dirs)
}

/// Refuse a sandbox whose root, state dir, tracker or config dir
/// overlaps a live dir in either direction — equal (the socket lives in
/// the state dir), nested inside it, or containing it (a `reset` would
/// delete it).
pub(super) fn refuse_production(sb: &Sandbox) -> Result<()> {
    refuse_overlap(sb, true, false)
}

/// The same overlap guard for a verb that does not delete: `status`,
/// `env` and the global gate in a shell exported by `cadence dev env`
/// name the sandbox's own dirs on both sides, and refusing that made the
/// sandbox unusable in its own environment (the CAD-832 acceptance
/// covers the gate).
///
/// Only an *exported* dir that is exactly one of the sandbox's own is
/// exempt; production's host-derived defaults keep their equality check,
/// so a sandbox rooted at the production tracker, home or config is still
/// refused. Every deleting verb — `reset`, `down`, `reload` — keeps the
/// strict inventory with no exemption at all: there, a marker planted at
/// a production path makes that path the sandbox's own, and only the
/// strict form refuses to delete it.
pub(super) fn refuse_production_readonly(sb: &Sandbox) -> Result<()> {
    refuse_overlap(sb, true, true)
}

fn refuse_overlap(sb: &Sandbox, exported: bool, own_exports_are_ours: bool) -> Result<()> {
    let ours = [
        sb.root.clone(),
        sb.state_dir(),
        sb.pm_dir(),
        sb.config_dir(),
    ]
    .map(|p| resolved(&p));
    for (label, dir) in production_dirs(sb, exported)? {
        let live = resolved(&dir);
        // An exported dir that is one of the sandbox's own paths — or
        // inside its own root, as every `CADENCE_HOME=<root>` path is —
        // is the sandbox, not a live cadence. A host-derived default at
        // the same path is still production and still refuses.
        // Only an export that is exactly one of the sandbox's own dirs is
        // the sandbox. A `CADENCE_HOME` is never exempt, not even when it
        // equals the candidate root: it is the name of a real
        // installation, and a marker that claims it must not authorize a
        // host-wide write.
        if own_exports_are_ours
            && label.starts_with("the exported ")
            && ours.iter().any(|own| own == &live)
        {
            continue;
        }
        if ours
            .iter()
            .any(|p| p.starts_with(&live) || live.starts_with(p))
        {
            // A shell pointed at this sandbox by `dev env` exports its own
            // roots, and the deleting verbs cannot tell that from a marker
            // planted at a live installation. The advice is conditional and
            // leads with the check: on a live install the "clean shell"
            // command below is exactly what deletes it.
            if label.starts_with("the exported ") && ours.iter().any(|own| own == &live) {
                let clean = "env -u CADENCE_STATE_DIR -u CADENCE_PM_DIR -u XDG_CONFIG_HOME -u CADENCE_PROFILE";
                return Err(Error::rejected(format!(
                    "sandbox '{name}' at {root} overlaps {label} {dir} — refusing; nothing was \
                     changed. This verb deletes or restarts the sandbox and accepts no export \
                     as proof. First check {root}: if it is a live installation (a tracker, a \
                     daemon database, hosted credentials), STOP — its marker is planted; run \
                     nothing below and remove the marker only by hand after you have looked. \
                     Only if this shell came from `cadence dev env` AND {root} is a disposable \
                     sandbox, run one of these from a clean shell: `{clean} cadence dev down \
                     {name}`, `{clean} cadence dev reload --name {name}`, or `{clean} cadence \
                     dev reset {name}`",
                    name = sb.name,
                    root = sb.root.display(),
                    dir = dir.display()
                )));
            }
            return Err(Error::rejected(format!(
                "sandbox '{}' at {} overlaps {label} {} — refusing; point \
                 CADENCE_SANDBOX_ROOT at a scratch dir, or `unset CADENCE_STATE_DIR \
                 CADENCE_PM_DIR` if this shell is exported at a live cadence",
                sb.name,
                sb.root.display(),
                dir.display()
            )));
        }
    }
    Ok(())
}

// ---------- marker ----------

/// The marker's JSON when the root holds one — refused when it names
/// another sandbox or is not a regular file.
pub(super) fn read_marker(sb: &Sandbox) -> Result<Option<Value>> {
    let path = sb.marker();
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::rejected(format!(
                "cannot read {} ({e}) — {} is not a sandbox root; pick another name",
                path.display(),
                sb.root.display()
            )))
        }
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(Error::rejected(format!(
                "{} is not a regular file — {} is not a sandbox root; \
                 pick another name",
                path.display(),
                sb.root.display()
            )))
        }
        Ok(_) => {}
    }
    let marker: Value = serde_json::from_str(&std::fs::read_to_string(&path)?).map_err(|e| {
        Error::rejected(format!(
            "{} is not valid JSON ({e}) — pick another name",
            path.display()
        ))
    })?;
    if marker["name"].as_str() != Some(sb.name.as_str()) {
        return Err(Error::rejected(format!(
            "{} belongs to sandbox {}, not '{}' — pick another name \
             (`cadence dev ls` lists them)",
            sb.root.display(),
            marker["name"],
            sb.name
        )));
    }
    Ok(Some(marker))
}

/// A root without a marker must be empty; otherwise `up` or `setup` would
/// treat unrelated contents as a sandbox (and `reset` could delete them).
pub(super) fn refuse_unmarked_nonempty(sb: &Sandbox, existing: Option<&Value>) -> Result<()> {
    if existing.is_some() {
        return Ok(());
    }
    match std::fs::read_dir(&sb.root) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(Error::rejected(format!(
                    "{} exists and holds no sandbox marker — pick another name",
                    sb.root.display()
                )));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::rejected(format!(
            "cannot read {} ({e}) — refusing; pick another name",
            sb.root.display()
        ))),
    }
}

/// `<root>/.cadence-sandbox` when `state_dir` is `<root>/state` beside a
/// marker — the one file `owner_of` must READ to judge the root, so a
/// confined process (the master, CAD-524) needs it in its filesystem
/// policy or every `cadence` verb refuses before dispatch. Presence
/// only: contents stay `read_marker`'s to reject, and granting the file
/// never widens to the root. `None` for any other layout, production's
/// included, so a caller can push it unconditionally.
pub fn marker_for(state_dir: &Path) -> Option<PathBuf> {
    if state_dir.file_name().and_then(|n| n.to_str()) != Some("state") {
        return None;
    }
    let marker = state_dir.parent()?.join(MARKER);
    std::fs::symlink_metadata(&marker).is_ok().then_some(marker)
}

/// The sandbox `state_dir` belongs to: `<root>/state` beside a marker
/// naming `<root>`. `None` for any other dir, production's included. A
/// marker that is present but unusable refuses rather than let the dir
/// run ungated — and so does a layout that only looks like a sandbox:
/// a symlinked `state`, one resolving anywhere but `<root>/state`, or
/// a root overlapping production's defaults. The marker is a
/// hand-writable file; it must never exempt a production database from
/// the rollout gates.
pub fn owner_of(state_dir: &Path) -> Result<Option<String>> {
    if state_dir.file_name().and_then(|n| n.to_str()) != Some("state") {
        return Ok(None);
    }
    let Some(root) = state_dir.parent() else {
        return Ok(None);
    };
    if std::fs::symlink_metadata(root.join(MARKER)).is_err() {
        return Ok(None);
    }
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let sb = Sandbox {
        base: root.parent().unwrap_or(root).to_path_buf(),
        root: root.to_path_buf(),
        name,
    };
    let owned = validate_name(&sb.name)
        .and_then(|()| require_marker(&sb))
        .and_then(|_| {
            let linked =
                std::fs::symlink_metadata(state_dir).is_ok_and(|m| m.file_type().is_symlink());
            if linked || resolved(state_dir) != resolved(root).join("state") {
                return Err(Error::rejected(format!(
                    "{} is not the root's own `state` directory",
                    state_dir.display()
                )));
            }
            refuse_overlap(&sb, false, false)
        });
    owned.map(|()| Some(sb.name.clone())).map_err(|e| {
        Error::rejected(format!(
            "{} sits in a sandbox root whose marker is unusable ({e}) — \
             refusing to run it ungated",
            state_dir.display()
        ))
    })
}

/// CAD-1187: the dev store `state_dir` is — the only kind of store that
/// may change build without the rollout lease. All of: it is
/// `<root>/state` beside a usable sandbox marker ([`owner_of`]); `<root>`
/// is a direct child of the sandbox base once symlinks resolve; and the
/// store holds a dev marker that names its own resolved path. `None`
/// otherwise — for production, for a plain `--state-dir`, for a marker
/// copied elsewhere. The caller's env (`CADENCE_PROFILE`) is not read.
pub fn dev_owner(state_dir: &Path) -> Result<Option<String>> {
    dev_owner_under(state_dir, base_dir().ok().as_deref())
}

pub(super) fn dev_owner_under(state_dir: &Path, base: Option<&Path>) -> Result<Option<String>> {
    let Some(name) = owner_of(state_dir)? else {
        return Ok(None);
    };
    let Some(sb) = Sandbox::from_state_dir(state_dir) else {
        return Ok(None);
    };
    if !base.is_some_and(|b| under_base(&sb, b)) || !dev_marker_ok(&sb, state_dir) {
        return Ok(None);
    }
    Ok(Some(name))
}

/// `<root>` is a direct child of the sandbox base, after symlinks.
fn under_base(sb: &Sandbox, base: &Path) -> bool {
    resolved(&sb.root).parent() == Some(resolved(base).as_path())
}

/// A regular, non-symlink `<state>/.cadence-dev` naming this sandbox
/// and this store's resolved path.
fn dev_marker_ok(sb: &Sandbox, state_dir: &Path) -> bool {
    let path = state_dir.join(DEV_MARKER);
    let regular = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file());
    if !regular {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(marker) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    marker["v"].as_u64() == Some(1)
        && marker["name"].as_str() == Some(sb.name.as_str())
        && marker["state_dir"].as_str() == resolved(state_dir).to_str()
}

pub(super) fn write_dev_marker(sb: &Sandbox) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let marker = json!({
        "v": 1,
        "name": sb.name,
        "state_dir": resolved(&sb.state_dir()),
    });
    let path = sb.dev_marker();
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — refusing to write the dev marker through it",
            path.display()
        )));
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&marker).unwrap_or_default() + "\n",
    )?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Run as the sandbox `state_dir` belongs to, whatever the caller's
/// env says: its profile, tracker and state dir go into this process's
/// env, so the daemon, the board and everything they spawn stay gated
/// — a bare `cadence --state-dir <root>/state daemon start` is still
/// the sandbox. Any other state dir is left alone.
pub fn adopt(state_dir: &Path) -> Result<Option<String>> {
    let Some(name) = owner_of(state_dir)? else {
        return Ok(None);
    };
    let root = state_dir.parent().unwrap_or(state_dir);
    std::env::set_var("CADENCE_PROFILE", format!("{PROFILE_PREFIX}{name}"));
    // CAD-1922: the adopted shell is the sandbox's home too — the
    // resolver maps the profile to this root — and XDG_CONFIG_HOME
    // keeps hosted config and credentials inside it. CADENCE_HOME is
    // deliberately left unset: the overlap guard must be free to read
    // it as evidence of a real installation.
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var("CADENCE_PM_DIR", root.join("pm"));
    std::env::set_var("CADENCE_STATE_DIR", state_dir);
    // Pin the base the *adopted* root came from, not whatever the ambient
    // environment says: the resolver rebuilds the root from the profile
    // name, and another exported base would select a different sandbox
    // with the same name.
    if let Some(base) = root.parent() {
        std::env::set_var("CADENCE_SANDBOX_ROOT", base);
    }
    Ok(Some(name))
}

pub(super) fn require_marker(sb: &Sandbox) -> Result<Value> {
    read_marker(sb)?.ok_or_else(|| {
        Error::rejected(format!(
            "no dev instance '{}' at {} — `cadence dev up {}` creates it",
            sb.name,
            sb.root.display(),
            sb.name
        ))
    })
}
