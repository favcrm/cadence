//! CAD-1019 slice 1a: a slim org registry — user preference, never authority.
//!
//! `orgs.json` in the resolved consolidated or legacy config root holds the
//! operator's org/connection selection. It is a preference file:
//! nothing in it is a credential, and remote authority is enforced
//! server-side by the device grant's audience/org binding. Writes therefore
//! carry **no operator proof** — a preference can be set by any process the
//! user's own permissions allow; what it can never do is grant membership,
//! mint a session or redirect a daemon verb.
//!
//! - `local` is a first-class org: the standalone local Cadence this host's
//!   state dir + tracker already are, no cloud identity asserted.
//! - `--org` beats `CADENCE_ORG` beats the saved `selected`; absent all
//!   three the resolved org is `local`.
//! - A genuinely absent registry is standalone local use. A *broken* one —
//!   an unusable declared config root, a dangling symlink (at the registry
//!   or in a config-root directory above it), an unreadable parent or file,
//!   a malformed `orgs.json` — refuses selection before any local state is
//!   used; it never falls back to ambient local roots. A symlinked config
//!   root that resolves to a directory stays usable.
//! - A managed caller (`CADENCE_ALIAS`) keeps its inherited local daemon
//!   and never consults the registry. `--org` is accepted only when it
//!   matches the hosted org on that alias's single running kickoff
//!   binding; any other selector stays refused. `org switch` stays refused.
//! - A `switch` mid-command cannot move it: each invocation resolves its
//!   destination once at startup.
//! - `Remote` rows persist the issuer-verified endpoint and org id but are
//!   refused by `resolve` — no transport in this slice, no local fallback.
//!
//! Adapted from PR #415 (`5e5a4c6a26ecc68de1719a77320868c04c20219c`) — the
//! operator-proof writes, `add`/`remove` verbs and per-connection selection
//! are removed per the CAD-1019 PM direction; the on-disk `orgs.json` shape
//! is unchanged so a later CAD-657 merge does not fork the format.

use super::{eprint_line, print_json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use cadence_agent::error::{Error, Result};

/// The always-present org: standalone local Cadence, materialized by
/// `ensure_local` on first `switch local` so a bare registry has an arm.
const LOCAL_ORG: &str = "local";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Selection {
    org: String,
}

/// `Local` carries the pair together so a selection never splits state and
/// tracker across roots (the CAD-657 finding). `Remote` is the forward-
/// compatible arm — `resolve` refuses it until the transport slice lands.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum Destination {
    Local {
        state_dir: PathBuf,
        tracker_dir: PathBuf,
    },
    Remote {
        endpoint: String,
        org_id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    selection: Selection,
    destination: Destination,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    selected: Option<Selection>,
    connections: Vec<Connection>,
}

struct RegistryFile {
    path: PathBuf,
    _lock: File,
}

impl RegistryFile {
    fn path() -> Result<PathBuf> {
        // Use the same first-run or legacy config root as hosted credentials.
        let root = cadence_agent::home::config_dir()?;
        if !root.is_absolute()
            || root.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return Err(Error::rejected(
                "the config root must be absolute with no . or ..",
            ));
        }
        Ok(root.join("orgs.json"))
    }
    /// Exclusive flock: every read-modify-write is one locked snapshot, so a
    /// `login` racing a `switch` cannot interleave a stale default.
    fn open() -> Result<Self> {
        let path = Self::path()?;
        let dir = path.parent().expect("registry parent");
        cadence_agent::home::create_home_dir(dir)?;
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join("orgs.lock"))?;
        if !lock.metadata()?.is_file() {
            return Err(Error::rejected("org registry lock must be a regular file"));
        }
        // SAFETY: flock acts only on the descriptor this guard owns.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self { path, _lock: lock })
    }
    fn read(&self) -> Result<Registry> {
        read_registry(&self.path)
    }
    fn read_snapshot(&self) -> Result<(Registry, Option<Vec<u8>>)> {
        read_registry_raw(&self.path)
    }
    fn write(&self, registry: &Registry) -> Result<()> {
        let raw =
            serde_json::to_vec_pretty(registry).map_err(|e| Error::internal(e.to_string()))?;
        self.write_raw(&raw)
    }
    fn write_raw(&self, raw: &[u8]) -> Result<()> {
        let dir = self.path.parent().expect("registry parent");
        if raw.len() > 65536 {
            return Err(Error::rejected(
                "org registry exceeds 64 KiB; no configuration changed",
            ));
        }
        let mut temp = tempfile::NamedTempFile::new_in(dir)?;
        temp.write_all(raw)?;
        temp.as_file().sync_all()?;
        // Once persist renames the registry, it is published. A directory
        // fsync is best-effort: reporting an error here would make login
        // skip the credential save with the new binding already installed.
        let dir_handle = File::open(dir).ok();
        temp.persist(&self.path)
            .map_err(|e| Error::internal(e.to_string()))?;
        if let Some(dir) = &dir_handle {
            let _ = dir.sync_all();
        }
        Ok(())
    }
    /// Put back the exact bytes read before a replacement, including a
    /// compact registry whose pretty form would exceed the write limit.
    /// `None` means the file did not exist.
    fn restore(&self, raw: Option<&[u8]>) -> Result<()> {
        let Some(raw) = raw else {
            match std::fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let dir = self.path.parent().expect("registry parent");
            if let Ok(dir) = File::open(dir) {
                let _ = dir.sync_all();
            }
            return Ok(());
        };
        self.write_raw(raw)
    }
}

/// The saved default org, if the registry is already there. A missing
/// registry is "no saved org"; a broken one — an unusable config root, a
/// dangling symlink, an unreadable or malformed file — refuses rather than
/// pretending nothing was ever selected. This does not create the
/// directory, the registry, or the lock, and it never changes the
/// selection.
pub(super) fn selected_org_readonly() -> Result<Option<String>> {
    let Some(path) = selection_registry_path()? else {
        return Ok(None);
    };
    if !registry_present(&path)? {
        return Ok(None);
    }
    Ok(read_present_registry(&path)?
        .selected
        .map(|selected| selected.org))
}

/// Read and validate `orgs.json` at `path` — the body of
/// `RegistryFile::read` split out so a read-only caller (`cadence
/// update` routing) shares the exact shape checks without taking the
/// write lock or creating the lock file.
fn read_registry(path: &Path) -> Result<Registry> {
    Ok(read_registry_raw(path)?.0)
}

/// Read the registry and keep the exact bytes read. The bytes let a failed
/// credential save restore a compact file that `to_vec_pretty` would refuse
/// at the write cap.
fn read_registry_raw(path: &Path) -> Result<(Registry, Option<Vec<u8>>)> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Registry::default(), None))
        }
        // `O_NOFOLLOW` refuses any symlink, dangling or not. Name that
        // plainly: a link is a registry this process will not read, never
        // "no registry".
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(Error::rejected(
                "org registry must not be a symbolic link; refusing destination selection",
            ))
        }
        Err(e) => return Err(e.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 65536 {
        return Err(Error::rejected(
            "org registry must be a regular file no larger than 64 KiB",
        ));
    }
    let mut raw = Vec::new();
    (&mut file).take(65537).read_to_end(&mut raw)?;
    if raw.len() > 65536 {
        return Err(Error::rejected("org registry exceeds 64 KiB"));
    }
    let registry: Registry = serde_json::from_slice(&raw)
        .map_err(|_| Error::rejected("invalid org registry; refusing destination selection"))?;
    for (i, c) in registry.connections.iter().enumerate() {
        cadence_agent::proto::identifier(&c.selection.org, "org")?;
        validate_destination(&c.destination)?;
        if registry.connections[..i]
            .iter()
            .any(|p| p.selection == c.selection)
        {
            return Err(Error::rejected("duplicate org in registry"));
        }
    }
    if let Some(selected) = &registry.selected {
        if !registry
            .connections
            .iter()
            .any(|c| &c.selection == selected)
        {
            return Err(Error::rejected("selected org is missing"));
        }
    }
    Ok((registry, Some(raw)))
}

fn absolute(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(Error::rejected(
            "org connection paths must be absolute with no . or ..",
        ));
    }
    Ok(())
}

fn validate_destination(destination: &Destination) -> Result<()> {
    match destination {
        Destination::Local {
            state_dir,
            tracker_dir,
        } => {
            absolute(state_dir)?;
            absolute(tracker_dir)?;
        }
        Destination::Remote { endpoint, org_id } => {
            let rest = endpoint
                .strip_prefix("https://")
                .ok_or_else(|| Error::rejected("remote endpoint must use HTTPS"))?;
            let host = rest.split('/').next().unwrap_or_default();
            if host.is_empty()
                || !host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".:-".contains(&b))
                || endpoint
                    .bytes()
                    .any(|b| b.is_ascii_control() || b"?#@".contains(&b))
            {
                return Err(Error::rejected(
                    "remote endpoint must not contain credentials, query, fragment or control characters",
                ));
            }
            // The issuer's workspace id: `[A-Za-z0-9_-]`, ≤ 200 — the same
            // shape `remote_auth`/`device_login` accept; the org *name*
            // (`--org`) keeps the tighter `proto::identifier` grammar.
            if org_id.is_empty()
                || org_id.len() > 200
                || !org_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return Err(Error::rejected("org-id must be a workspace-style id"));
            }
        }
    }
    Ok(())
}

/// Pick the connection `org` names, else the saved default. `None` means
/// "no org selected" — the caller uses ambient local roots.
fn select(registry: &Registry, org: Option<&str>) -> Result<Option<Connection>> {
    let Some(org) = org.or_else(|| registry.selected.as_ref().map(|s| s.org.as_str())) else {
        return Ok(None);
    };
    registry
        .connections
        .iter()
        .find(|c| c.selection.org == org)
        .cloned()
        .map(Some)
        .ok_or_else(|| unknown_org(org, true))
}

fn unknown_org(org: &str, registered: bool) -> Error {
    let where_ = if registered {
        "is not in the org registry"
    } else {
        "has no org registry"
    };
    Error::rejected(format!(
        "org '{org}' {where_} — a remote org needs `cadence login` on this host; \
         run `cadence org list`, pass `--org local` for standalone use, or unset \
         CADENCE_ORG. An explicit destination never falls back to local state; nothing was sent"
    ))
}

/// `login_status` keeps its legacy values for existing consumers: `Local`
/// stays `local_peer_authority`; `Remote` is no longer always `not_connected`.
/// The honest, local-only detail is `credential` (see `remote_cli::local_status`):
/// its `state` says what this machine holds, and `live_verified` is always
/// `false` because `org list` makes no network call.
fn view(c: &Connection, selected: bool) -> Value {
    let (login_status, credential) = match &c.destination {
        Destination::Local { .. } => ("local_peer_authority", None),
        Destination::Remote { endpoint, org_id } => {
            let target = cadence_agent::remote_cli::RemoteTarget {
                identity: None,
                org: c.selection.org.clone(),
                endpoint: endpoint.clone(),
                org_id: org_id.clone(),
            };
            let status = cadence_agent::remote_cli::local_status(&target);
            // Only a proven absence keeps the legacy `not_connected`. An
            // unavailable or invalid local store is not "not connected".
            let legacy = if status["state"] == "no_credential" {
                "not_connected"
            } else {
                "credential_saved"
            };
            (legacy, Some(status))
        }
    };
    let mut out = json!({
        "org": c.selection.org,
        "selected": selected,
        "destination": c.destination,
        "login_status": login_status,
    });
    if let Some(credential) = credential {
        out["credential"] = credential;
    }
    out
}

/// Avoid accidental default changes in an inherited managed session.
/// Environment markers are not authentication or a security boundary.
pub(super) fn refuse_managed_default_change() -> Result<()> {
    if std::env::var_os("CADENCE_ALIAS").is_some()
        || std::env::var_os("CADENCE_GRANT_TOKEN").is_some()
    {
        return Err(Error::rejected(
            "a managed session must not change the saved workspace default; \
             use --org <workspace> for this command or ask the operator to change the default",
        ));
    }
    Ok(())
}

/// `cadence org …` — no operator proof: a preference, never authority.
pub(super) fn run(action: OrgAction) -> Result<i32> {
    if matches!(&action, OrgAction::Switch { .. }) {
        refuse_managed_default_change()?;
    }
    // `switch local` materializes the first-class connection before the
    // shared read, so `ensure_local`'s write lock never nests under this one.
    if matches!(&action, OrgAction::Switch { name } if name == LOCAL_ORG) {
        ensure_local(
            &cadence_agent::client::state_dir()?,
            &cadence_agent::issue::default_dir()?,
        )?;
    }
    let file = RegistryFile::open()?;
    let mut registry = file.read()?;
    let out = match action {
        OrgAction::List => json!({
            "connections": registry
                .connections
                .iter()
                .map(|c| view(c, registry.selected.as_ref() == Some(&c.selection)))
                .collect::<Vec<_>>()
        }),
        OrgAction::Inspect { name } => {
            let name = name
                .as_deref()
                .map(str::to_string)
                .or_else(|| std::env::var("CADENCE_ORG").ok());
            let c = select(&registry, name.as_deref())?
                .ok_or_else(|| Error::rejected("no org selected"))?;
            view(&c, registry.selected.as_ref() == Some(&c.selection))
        }
        OrgAction::Switch { name } => {
            let c = select(&registry, Some(&name))?.expect("explicit org");
            registry.selected = Some(c.selection.clone());
            file.write(&registry)?;
            view(&c, true)
        }
    };
    print_json(&out);
    Ok(0)
}

/// Write the `local` connection once, bound to this host's resolved roots.
/// `switch local` calls it so a bare registry still has the arm.
fn ensure_local(state_dir: &Path, tracker_dir: &Path) -> Result<()> {
    let file = RegistryFile::open()?;
    let mut registry = file.read()?;
    if registry
        .connections
        .iter()
        .any(|c| c.selection.org == LOCAL_ORG)
    {
        return Ok(());
    }
    let destination = Destination::Local {
        state_dir: state_dir.to_path_buf(),
        tracker_dir: tracker_dir.to_path_buf(),
    };
    validate_destination(&destination)?;
    registry.connections.push(Connection {
        selection: Selection {
            org: LOCAL_ORG.to_string(),
        },
        destination,
    });
    file.write(&registry)
}

/// Pure registry mutation for `cadence login` (CAD-1019 slice 1b), unit-tested
/// without the registry file. `org_name` is the issuer-verified slug and
/// `endpoint`/`org_id` come only from the verified grant. A newer verified
/// grant replaces a stale remote binding for the same slug (a workspace can
/// be recreated or its id rotated); a `local` row is never overwritten. The
/// new org is selected only when `use_default` (`--use`) is passed — a bare
/// first login leaves `selected` unset so ambient local stays the default
/// (CAD-1125).
fn apply_record_remote(
    registry: &mut Registry,
    org_name: &str,
    endpoint: &str,
    org_id: &str,
    use_default: bool,
) -> Result<(Connection, bool)> {
    let org = cadence_agent::proto::identifier(org_name, "org")?;
    let destination = Destination::Remote {
        endpoint: endpoint.to_string(),
        org_id: org_id.to_string(),
    };
    validate_destination(&destination)?;
    let selection = Selection { org: org.clone() };
    if let Some(existing) = registry
        .connections
        .iter_mut()
        .find(|c| c.selection == selection)
    {
        match &existing.destination {
            Destination::Remote { .. } => {
                // The grant is issuer-verified and already validated above;
                // re-login is the operator's replacement intent.
                existing.destination = destination.clone();
            }
            Destination::Local { .. } => {
                return Err(Error::rejected(
                    "an org with this name exists as a local connection; pick another name",
                ));
            }
        }
    } else {
        registry.connections.push(Connection {
            selection: selection.clone(),
            destination: destination.clone(),
        });
    }
    // CAD-1125: only `--use` moves the saved default. A first login on a
    // registry with no selection leaves `selected` unset — ambient local
    // stays the default; a bare remote login must not silently retarget
    // unmanaged commands at the cloud.
    let selected = if use_default {
        registry.selected = Some(selection.clone());
        true
    } else {
        registry.selected.as_ref() == Some(&selection)
    };
    Ok((
        Connection {
            selection,
            destination,
        },
        selected,
    ))
}

/// File-locked wrapper over `apply_record_remote`. The registry is written
/// first and `commit` (the credential save) runs while the registry lock is
/// still held; a failed commit restores the previous binding. Returns the
/// stored view.
pub(super) fn record_remote<F>(
    org_name: &str,
    endpoint: &str,
    org_id: &str,
    use_default: bool,
    commit: F,
) -> Result<Value>
where
    F: FnOnce() -> Result<()>,
{
    let file = RegistryFile::open()?;
    let (mut registry, previous_raw) = file.read_snapshot()?;
    let (connection, selected) =
        apply_record_remote(&mut registry, org_name, endpoint, org_id, use_default)?;
    file.write(&registry)?;
    if let Err(error) = commit() {
        if let Err(rollback) = file.restore(previous_raw.as_deref()) {
            return Err(Error::internal(format!(
                "credential write failed ({error}); restoring the previous org registry also failed ({rollback})"
            )));
        }
        return Err(error);
    }
    Ok(view(&connection, selected))
}

/// What `resolve` settled for this invocation: a local daemon state dir,
/// or a pinned remote destination (CAD-1019 slice 2). `Remote` carries the
/// issuer-verified endpoint + workspace — never a credential, and never
/// re-derived after this call.
#[derive(Debug)]
pub(super) enum Resolved {
    Local(PathBuf),
    Remote(cadence_agent::remote_cli::RemoteTarget),
}

/// The destination alone, for a verb whose local arm needs no state dir:
/// `audit verdicts` parses a notes directory and must answer with no `HOME`,
/// no daemon and no registry write. Precedence, pins, the managed binding and
/// every refusal are [`settle`]'s, so this never disagrees with [`resolve`].
#[derive(Debug)]
pub(super) enum Routed {
    Local,
    Remote(cadence_agent::remote_cli::RemoteTarget),
}

/// [`settle`]'s answer before projection. `Local` carries only an explicit
/// pin — the ambient state dir is materialized by [`resolve`], never by a
/// caller that does not need one.
enum Selected {
    Local {
        root: Option<PathBuf>,
    },
    /// A registry `local` connection: its pair is already explicit, and
    /// selecting it exports state + tracker (CAD-657).
    LocalConnection {
        org: String,
        state_dir: PathBuf,
        tracker_dir: PathBuf,
    },
    Remote(cadence_agent::remote_cli::RemoteTarget),
}

/// How a settlement reads the org registry. `Locked` takes the exclusive lock
/// (creating the directory and `orgs.lock` when the registry is there), which
/// is what a verb that may write, or that must serialize against one, has
/// always done. `ReadOnly` shares the same shape checks through
/// [`read_registry_raw`] and creates nothing, so a pure read works on a
/// read-only config directory and leaves no lock file behind.
enum RegistryMode {
    Locked,
    ReadOnly,
}

fn managed_org_refused() -> Error {
    Error::rejected("managed callers cannot override their destination with --org")
}

fn pin_conflict() -> Error {
    Error::rejected(
        "--org conflicts with an explicit or inherited local instance binding; \
         use a clean operator shell",
    )
}

/// `action_dispatch` forwards inherited `CADENCE_PM_DIR` as `tracker`.
/// That is the local daemon's tracker, not a second explicit pin. A
/// different path still conflicts with `--org`.
fn tracker_pin_conflicts(tracker: Option<&Path>) -> bool {
    let Some(tracker) = tracker else {
        return false;
    };
    !matches!(
        std::env::var_os("CADENCE_PM_DIR"),
        Some(inherited) if Path::new(&inherited) == tracker
    )
}

/// The one executing message, or `None` when there is not exactly one
/// `running` row with an id. Queued, terminal, and unresolved rows do
/// not qualify; a second running row is ambiguous.
fn single_running_message(show: &Value) -> Option<String> {
    let messages = show.get("messages")?.as_array()?;
    let mut found = None;
    for message in messages {
        if message.get("state").and_then(Value::as_str) != Some("running") {
            continue;
        }
        let id = message.get("id").and_then(Value::as_str)?;
        if found.is_some() {
            return None;
        }
        found = Some(id.to_string());
    }
    found
}

fn hosted_assignment_target(
    state_dir: &Path,
    alias: &str,
    selector: &str,
) -> Result<cadence_agent::remote_cli::RemoteTarget> {
    let show = cadence_agent::client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
    let Some(message_id) = single_running_message(&show) else {
        return Err(managed_org_refused());
    };
    let bound =
        cadence_agent::local_assignment::reporting::hosted_binding_target(state_dir, &message_id)?;
    let Some(bound) = bound else {
        return Err(managed_org_refused());
    };
    if bound.org != selector {
        return Err(managed_org_refused());
    }
    let target = cadence_agent::remote_cli::RemoteTarget {
        identity: Some(cadence_agent::remote_cli::CliIdentityPin {
            subject_id: bound.subject_id,
            issuer: bound.issuer,
        }),
        org: bound.org,
        endpoint: bound.endpoint,
        org_id: bound.organization_id,
    };
    let dir = cadence_agent::remote_auth::auth_dir(None).map_err(|_| {
        Error::rejected("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    cadence_agent::remote_cli::cli_material(&dir, &target)?;
    Ok(target)
}

fn settle_managed(
    org: Option<&str>,
    state: Option<PathBuf>,
    tracker: Option<PathBuf>,
) -> Result<Selected> {
    let Some(selector) = org else {
        return Ok(Selected::Local { root: state });
    };
    if state.is_some() || tracker_pin_conflicts(tracker.as_deref()) {
        return Err(pin_conflict());
    }
    let alias = std::env::var("CADENCE_ALIAS").map_err(|_| managed_org_refused())?;
    if alias.is_empty() {
        return Err(managed_org_refused());
    }
    let state_dir = cadence_agent::client::state_dir()?;
    let target = hosted_assignment_target(&state_dir, &alias, selector)?;
    eprint_line(&format!(
        "cadence org={} remote={}",
        target.org, target.endpoint
    ));
    Ok(Selected::Remote(target))
}

/// The saved login's bearer for the selected remote org, or `None` when this
/// machine is on a local org or holds no usable credential.
///
/// `None` keeps the GitHub device flow anonymous — the owner then chooses the
/// workspace explicitly — so a missing or unrenewable login never blocks the
/// flow and never invents a workspace. A broken registry (unusable config
/// root, dangling symlink, unreadable or malformed file) is not "no saved
/// login" and refuses instead.
pub(super) fn saved_login_token(auth_dir: Option<&Path>) -> Result<Option<String>> {
    let Some(org) = selected_org_readonly()? else {
        return Ok(None);
    };
    if org == LOCAL_ORG {
        return Ok(None);
    }
    let Some(path) = selection_registry_path()? else {
        return Ok(None);
    };
    if !registry_present(&path)? {
        return Ok(None);
    }
    let file = RegistryFile::open()?;
    let registry = present_registry(file.read_snapshot()?)?;
    let Some(conn) = select(&registry, Some(org.as_str()))? else {
        return Ok(None);
    };
    let Destination::Remote { endpoint, org_id } = conn.destination else {
        return Ok(None);
    };
    let target = cadence_agent::remote_cli::RemoteTarget {
        identity: None,
        org: conn.selection.org,
        endpoint,
        org_id,
    };
    let dir = cadence_agent::remote_auth::auth_dir(auth_dir)?;
    Ok(cadence_agent::remote_cli::cli_material(&dir, &target)
        .ok()
        .map(|material| material.access_token))
}

/// Whether this shell is bound to a local Cadence installation. The login
/// handover uses the same pin check as routing for its next command.
pub(super) fn inherited_local_pin() -> bool {
    [
        "CADENCE_STATE_DIR",
        "CADENCE_PM_DIR",
        "CADENCE_HOME",
        "CADENCE_PROFILE",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some())
}

/// Settle this invocation's destination once — the shared body of [`resolve`]
/// and [`route`], so the two can never disagree. `org` is `--org`. A
/// managed caller keeps its inherited local daemon and never consults
/// the registry. `--org` is accepted only when the live daemon reports
/// exactly one running message whose kickoff binding names that org and
/// whose original member and issuer still match the saved login;
/// endpoint and organization id come from that binding. A binding with
/// no member pin stays unavailable. An explicit
/// `--state-dir`, or a tracker path other than the inherited
/// `CADENCE_PM_DIR`, still conflicts. For every other caller,
/// `state`/`tracker` are pins — either wins over the registry and
/// refuses `--org`. Never resolves the tracker for a managed, pinned or
/// ambient caller — those never needed it (the CAD-313 `ui login`
/// residual runs with no `HOME`).
fn settle(
    org: Option<&str>,
    state: Option<PathBuf>,
    tracker: Option<PathBuf>,
    mode: RegistryMode,
) -> Result<Selected> {
    if std::env::var_os("CADENCE_ALIAS").is_some() {
        return settle_managed(org, state, tracker);
    }
    // An explicit flag or inherited binding is a pin; it conflicts with
    // `--org` rather than mixing roots.
    let pinned = state.is_some() || tracker.is_some() || inherited_local_pin();
    if pinned {
        if org.is_some() {
            return Err(pin_conflict());
        }
        return Ok(Selected::Local { root: state });
    }
    let org_env = std::env::var("CADENCE_ORG").ok();
    let org = org.or(org_env.as_deref());
    // No org anywhere and no registry → ambient local; the registry never
    // has to exist for standalone local use.
    let registry = read_selection_registry(mode)?;
    let selection = match (&registry, org) {
        (None, Some(name)) if name != LOCAL_ORG => return Err(unknown_org(name, false)),
        (Some(registry), _) => {
            if org == Some(LOCAL_ORG) {
                registry
                    .connections
                    .iter()
                    .find(|c| c.selection.org == LOCAL_ORG)
                    .cloned()
            } else {
                select(registry, org)?
            }
        }
        _ => None,
    };
    let Some(conn) = selection else {
        return Ok(Selected::Local { root: None });
    };
    match conn.destination {
        Destination::Local {
            state_dir,
            tracker_dir,
        } => Ok(Selected::LocalConnection {
            org: conn.selection.org,
            state_dir,
            tracker_dir,
        }),
        Destination::Remote { endpoint, org_id } => {
            // The destination is the issuer-verified endpoint recorded at
            // login — `remote_cli` refuses to send this org's credential
            // anywhere else, and there is no local fallback.
            eprint_line(&format!(
                "cadence org={} remote={}",
                conn.selection.org, endpoint
            ));
            Ok(Selected::Remote(cadence_agent::remote_cli::RemoteTarget {
                identity: None,
                org: conn.selection.org,
                endpoint,
                org_id,
            }))
        }
    }
}

/// The registry a settlement reads. `Locked` keeps the exclusive-lock read
/// every other verb uses. `ReadOnly` answers a pure read through the same shape
/// checks without taking or creating `orgs.lock`. Both fail closed: only a
/// genuinely absent first-use path is "no registry" — a dangling symlink
/// (including one in the config root above the registry), a permission error
/// and a non-file all refuse. A declared but unusable config, home or sandbox
/// root is a broken selection, not an absent one. With no declared root at
/// all, a local read still answers.
fn read_selection_registry(mode: RegistryMode) -> Result<Option<Registry>> {
    let Some(path) = selection_registry_path()? else {
        return Ok(None);
    };
    // The presence gate is shared: a broken config root must refuse before a
    // read-only verb can treat it as "no registry" too.
    if !registry_present(&path)? {
        return Ok(None);
    }
    match mode {
        // Unchanged for every other verb: a genuinely absent registry creates
        // nothing — not the directory, not the lock — while a present one is
        // read under the exclusive write lock.
        RegistryMode::Locked => Ok(Some(present_registry(
            RegistryFile::open()?.read_snapshot()?,
        )?)),
        RegistryMode::ReadOnly => Ok(Some(read_present_registry(&path)?)),
    }
}

/// The registry path this process would use, or `None` when it declared no
/// home, config root or sandbox. A declared but unusable root is a broken
/// selection and refuses before any local state is materialized.
fn selection_registry_path() -> Result<Option<PathBuf>> {
    match RegistryFile::path() {
        Ok(path) => Ok(Some(path)),
        Err(_) if !config_root_declared() => Ok(None),
        Err(e) => Err(Error::rejected(format!(
            "cannot locate the org registry: {e}"
        ))),
    }
}

/// Whether `orgs.json` is there, decided without following the registry
/// itself, without turning an I/O failure into "absent", and without writing
/// anything.
///
/// A single `symlink_metadata(path)` is not enough: it reports `NotFound` for
/// everything *below* a dangling symlink too, so a `XDG_CONFIG_HOME`, a
/// `HOME/.config` or a `cadence` directory that is a link to a missing target
/// would read as first use and let selection fall back to ambient local state.
/// When `path` itself is missing, the parent chain decides: the first ancestor
/// that exists must be a usable directory. A real directory — or a symlink to
/// one, which keeps a legitimately relocated config root working — means every
/// missing component below it is genuinely absent, so this is first use. A
/// dangling symlink, a non-directory or an unreadable component is a broken
/// selection and refuses. `read_registry_raw` then refuses a symlink at the
/// registry itself through `O_NOFOLLOW`.
fn registry_present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(inspect_refused(&e.to_string())),
    }
    for ancestor in path.ancestors().skip(1) {
        let metadata = match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => metadata,
            // This component does not exist; the next ancestor up decides
            // whether that is first use or a dangling link above it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(inspect_refused(&e.to_string())),
        };
        let shown = ancestor.display();
        if metadata.is_dir() {
            return Ok(false);
        }
        if !metadata.file_type().is_symlink() {
            return Err(inspect_refused(&format!("{shown} is not a directory")));
        }
        // A symlinked config root is legitimate when it resolves to a
        // directory; a dangling, looping or non-directory target is broken.
        match std::fs::metadata(ancestor) {
            Ok(target) if target.is_dir() => return Ok(false),
            Ok(_) => return Err(inspect_refused(&format!("{shown} is not a directory"))),
            Err(e) => return Err(inspect_refused(&format!("{shown} cannot be followed: {e}"))),
        }
    }
    // Every ancestor was missing. The filesystem root always exists, so this
    // is reachable only for a relative path, which `RegistryFile::path`
    // never produces; a relative path cannot hold a registry this process
    // would trust either.
    Ok(false)
}

fn inspect_refused(detail: &str) -> Error {
    Error::rejected(format!("cannot inspect the org registry: {detail}"))
}

/// The registry [`registry_present`] just reported as there is gone. That is a
/// race with a concurrent removal, not first use, so it refuses instead of
/// answering "no registry" and quietly falling back to ambient local state.
fn vanished_registry() -> Error {
    Error::rejected(
        "org registry disappeared while selecting a destination; refusing destination selection",
    )
}

/// The registry behind a presence check that already passed.
fn present_registry(snapshot: (Registry, Option<Vec<u8>>)) -> Result<Registry> {
    match snapshot {
        (registry, Some(_)) => Ok(registry),
        (_, None) => Err(vanished_registry()),
    }
}

fn read_present_registry(path: &Path) -> Result<Registry> {
    present_registry(read_registry_raw(path)?)
}

/// Whether this process declared a home or config root, including a sandbox.
/// A declared root that cannot be resolved is broken, not an absent registry.
fn config_root_declared() -> bool {
    ["XDG_CONFIG_HOME", "HOME", "CADENCE_HOME"]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
        || cadence_agent::sandbox::profile().is_some()
}

/// The local state dir a caller that needs one materializes: an explicit pin,
/// else the ambient `CADENCE_STATE_DIR`/home default. [`route`] never calls
/// this, so a local read never needs `HOME`.
fn local_root(root: Option<PathBuf>) -> Result<PathBuf> {
    root.map(Ok)
        .unwrap_or_else(cadence_agent::client::state_dir)
}

/// Resolve this invocation's destination once, materializing the local state
/// dir and exporting a local connection's state + tracker pair. Precedence,
/// pins and refusals are [`settle`]'s.
pub(super) fn resolve(
    org: Option<&str>,
    state: Option<PathBuf>,
    tracker: Option<PathBuf>,
) -> Result<Resolved> {
    match settle(org, state, tracker, RegistryMode::Locked)? {
        Selected::Local { root } => local_root(root).map(Resolved::Local),
        Selected::LocalConnection {
            org,
            state_dir,
            tracker_dir,
        } => local_resolved(&org, state_dir, tracker_dir),
        Selected::Remote(target) => Ok(Resolved::Remote(target)),
    }
}

/// [`resolve`]'s destination only, for a verb whose local arm needs no state
/// dir: `audit verdicts` parses a notes directory and must answer with no
/// `HOME`, no daemon, and no write — the registry is read without taking or
/// creating `orgs.lock`, so the config directory is left untouched. An
/// explicit unknown org, a pin conflict and a managed override still refuse
/// exactly as [`resolve`] does, and a hosted selection is still the org's own
/// destination — never a local fallback.
pub(super) fn route(
    org: Option<&str>,
    state: Option<PathBuf>,
    tracker: Option<PathBuf>,
) -> Result<Routed> {
    Ok(match settle(org, state, tracker, RegistryMode::ReadOnly)? {
        Selected::Local { .. } | Selected::LocalConnection { .. } => Routed::Local,
        Selected::Remote(target) => Routed::Remote(target),
    })
}

/// Shared `Destination::Local` routing: export the pair so state and
/// tracker land on the org's roots, then return the state dir.
fn local_resolved(org: &str, state_dir: PathBuf, tracker_dir: PathBuf) -> Result<Resolved> {
    std::env::set_var("CADENCE_STATE_DIR", &state_dir);
    std::env::set_var("CADENCE_PM_DIR", &tracker_dir);
    eprint_line(&format!("cadence org={org} local={}", state_dir.display()));
    Ok(Resolved::Local(state_dir))
}

/// Which local-maintenance command is resolving, so a remote selector can
/// name that command.
#[derive(Clone, Copy)]
enum LocalMaintenance {
    Update,
    Dev,
    Setup,
    SkillRead,
}

impl LocalMaintenance {
    fn remote_rejected(self, name: &str) -> Error {
        match self {
            // Kept byte-for-byte with the CAD-1355 update refusal.
            Self::Update => Error::rejected(format!(
                "`cadence update` maintains a local install only — org '{name}' \
                 is remote. Run it on the host whose install you mean to update \
                 (a local selection such as `CADENCE_ORG=local` works there)."
            )),
            Self::Dev => Error::rejected(format!(
                "`cadence dev` runs a local disposable Cadence only — org '{name}' \
                 is remote. A saved remote org does not select it. Omit the remote \
                 `--org`/`CADENCE_ORG`, or choose local (for example `CADENCE_ORG=local`)."
            )),
            Self::Setup => Error::rejected(format!(
                "`cadence setup` installs on this machine only — org '{name}' is \
                 remote. Its hosted routes are its own `--hosted`/`--register` flags; \
                 omit the remote `--org`/`CADENCE_ORG`, or choose local."
            )),
            Self::SkillRead => Error::rejected(format!(
                "`cadence skill status` and `cadence skill show` inspect this machine's \
                 guidance, not remote org '{name}'. Omit the remote `--org`/`CADENCE_ORG`."
            )),
        }
    }

    /// `update` keeps its pin arm: an explicit root wins and `CADENCE_ORG`
    /// is not consulted. For `dev`, `setup`, and skill reads, a remote
    /// `CADENCE_ORG` beside that root is deliberate and must not be discarded.
    fn refuse_remote_env_beside_pins(self) -> bool {
        matches!(self, Self::Dev | Self::Setup | Self::SkillRead)
    }
}

fn maintenance_pins(state: Option<&Path>, tracker: Option<&Path>) -> bool {
    state.is_some()
        || tracker.is_some()
        || [
            "CADENCE_STATE_DIR",
            "CADENCE_PM_DIR",
            "CADENCE_HOME",
            "CADENCE_PROFILE",
        ]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
}

/// `Some(name)` when `CADENCE_ORG` names a stored remote connection.
/// `local`, a local connection, an unknown name, or a genuinely missing
/// registry are not remote. A present but unreadable registry, a dangling
/// symlink and an unusable config root fail closed: "cannot tell" is never
/// "not remote", which would quietly route local maintenance.
fn remote_org_env() -> Result<Option<String>> {
    let Some(name) = std::env::var("CADENCE_ORG").ok() else {
        return Ok(None);
    };
    if name == LOCAL_ORG {
        return Ok(None);
    }
    let Some(path) = selection_registry_path()? else {
        return Ok(None);
    };
    if !registry_present(&path)? {
        return Ok(None);
    }
    let registry = read_present_registry(&path)?;
    Ok(registry
        .connections
        .iter()
        .find(|c| c.selection.org == name)
        .and_then(|c| match c.destination {
            Destination::Remote { .. } => Some(name),
            Destination::Local { .. } => None,
        }))
}

/// CAD-1355: `cadence update` is local maintenance — a *saved* remote
/// default must never route it. This resolves only for the updater:
/// a managed caller keeps the pre-assignment rule (no `--org`, inherited
/// local state, no registry), pins behave exactly as [`resolve`], explicit
/// `--org`/`CADENCE_ORG` selectors stay deliberate (a remote one refuses
/// with local-only guidance before any transport, credential read or
/// container wake; a local one keeps the normal local routing), and a
/// bare invocation ignores the registry's `selected` row entirely — it
/// binds the canonical `local` connection when one exists, else the
/// ambient state dir. The registry is only ever read here: no row is
/// created, selected or switched to make updating work, and the tracker
/// pin precedence is unchanged. A live hosted assignment does not route
/// `update`.
pub(super) fn resolve_update(
    org: Option<&str>,
    state: Option<PathBuf>,
    tracker: Option<PathBuf>,
) -> Result<Resolved> {
    resolve_local_maintenance(LocalMaintenance::Update, org, state, tracker)
}

/// `cadence setup` installs on this machine. It shares the local-maintenance
/// selector, so a saved *local* connection keeps its state and tracker, a
/// saved remote default never routes it over the allowlist, and a remote
/// `--org`/`CADENCE_ORG` is refused rather than silently ignored.
pub(super) fn resolve_setup(
    org: Option<&str>,
    state: Option<PathBuf>,
    tracker: Option<PathBuf>,
) -> Result<Resolved> {
    resolve_local_maintenance(LocalMaintenance::Setup, org, state, tracker)
}

/// `cadence dev` (`sandbox` alias) is local maintenance of a disposable
/// Cadence. Same selector rules as [`resolve_update`], plus: a remote
/// `CADENCE_ORG` is refused even when a local pin or managed inherited
/// root is present, so that env is not silently discarded. `--org` on a
/// pin still conflicts exactly as [`resolve`]; `--org` on a managed caller
/// is still the managed-caller refusal, before any assignment lookup.
pub(super) fn resolve_dev(
    org: Option<&str>,
    state: Option<PathBuf>,
    tracker: Option<PathBuf>,
) -> Result<Resolved> {
    resolve_local_maintenance(LocalMaintenance::Dev, org, state, tracker)
}

/// Read-only skill discovery is local to the installation. Like `dev`, a
/// saved remote default does not select it, but a deliberate remote
/// selector is refused rather than silently discarded.
pub(super) fn resolve_skill_read(
    org: Option<&str>,
    state: Option<PathBuf>,
    tracker: Option<PathBuf>,
) -> Result<Resolved> {
    resolve_local_maintenance(LocalMaintenance::SkillRead, org, state, tracker)
}

fn resolve_local_maintenance(
    command: LocalMaintenance,
    org: Option<&str>,
    state: Option<PathBuf>,
    tracker: Option<PathBuf>,
) -> Result<Resolved> {
    // Local maintenance never follows a hosted assignment selector.
    if std::env::var_os("CADENCE_ALIAS").is_some() {
        if org.is_some() {
            return Err(managed_org_refused());
        }
        if command.refuse_remote_env_beside_pins() {
            if let Some(name) = remote_org_env()? {
                return Err(command.remote_rejected(&name));
            }
        }
        return state
            .map(Resolved::Local)
            .map(Ok)
            .unwrap_or_else(|| cadence_agent::client::state_dir().map(Resolved::Local));
    }
    if maintenance_pins(state.as_deref(), tracker.as_deref()) {
        // A remote env beside a local pin is deliberate. `--org` is left
        // to [`resolve`], which refuses the pin conflict before transport.
        if org.is_none() && command.refuse_remote_env_beside_pins() {
            if let Some(name) = remote_org_env()? {
                return Err(command.remote_rejected(&name));
            }
        }
        return resolve(org, state, tracker);
    }
    let org_env = std::env::var("CADENCE_ORG").ok();
    // Read-only: no write lock, no lock file, no created directory — the
    // updater never mutates the registry to reach its local install. Only a
    // genuinely missing registry means "no rows"; a broken one refuses before
    // any local state is chosen.
    let registry = match selection_registry_path()? {
        Some(path) => {
            if registry_present(&path)? {
                read_present_registry(&path)?
            } else {
                Registry::default()
            }
        }
        None => Registry::default(),
    };
    match org.or(org_env.as_deref()) {
        // A deliberate selector is honored or refused — never ignored.
        Some(name) => {
            let conn = registry
                .connections
                .iter()
                .find(|c| c.selection.org == name)
                .cloned();
            match conn.map(|c| c.destination) {
                Some(Destination::Local {
                    state_dir,
                    tracker_dir,
                }) => local_resolved(name, state_dir, tracker_dir),
                // `local` is first-class even without a materialized row.
                None if name == LOCAL_ORG => {
                    cadence_agent::client::state_dir().map(Resolved::Local)
                }
                None => Err(Error::rejected("unknown org")),
                Some(Destination::Remote { .. }) => Err(command.remote_rejected(name)),
            }
        }
        // No explicit selector: the saved default is a preference for
        // remote verbs, not for local maintenance — ignore it. Bind the
        // canonical `local` row when it exists; otherwise the ambient
        // state dir. Nothing is created, written or switched.
        None => match registry
            .connections
            .iter()
            .find(|c| c.selection.org == LOCAL_ORG)
            .cloned()
        {
            Some(Connection {
                selection,
                destination:
                    Destination::Local {
                        state_dir,
                        tracker_dir,
                    },
            }) => local_resolved(&selection.org, state_dir, tracker_dir),
            _ => cadence_agent::client::state_dir().map(Resolved::Local),
        },
    }
}

/// `cadence org` subcommands (see `run`).
#[derive(clap::Subcommand)]
pub(crate) enum OrgAction {
    /// Show registered orgs — destination and selection, never a credential.
    #[command(alias = "ls")]
    List,
    /// Change the saved default org. Refused under a managed caller.
    Switch {
        /// Org name — `local` selects standalone local Cadence.
        name: String,
    },
    /// Show one org, or the resolved default.
    Inspect {
        /// Org name; default is the saved/`CADENCE_ORG` selection.
        name: Option<String>,
    },
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    pub(in crate::cli) const EP_A: &str = "https://alpha.cadencecloud.app";
    const EP_B: &str = "https://beta.cadencecloud.app";

    pub(in crate::cli) static RESOLVE_ENV: Mutex<()> = Mutex::new(());

    pub(in crate::cli) struct EnvGuard {
        home: tempfile::TempDir,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        pub(in crate::cli) fn bare() -> Self {
            let home = tempfile::tempdir().unwrap();
            let saved = [
                "HOME",
                "XDG_CONFIG_HOME",
                "XDG_STATE_HOME",
                "CADENCE_ALIAS",
                "CADENCE_ORG",
                "CADENCE_STATE_DIR",
                "CADENCE_PM_DIR",
                "CADENCE_HOME",
                "CADENCE_PROFILE",
                "CADENCE_SOCKET",
            ]
            .into_iter()
            .map(|name| {
                let was = std::env::var_os(name);
                std::env::remove_var(name);
                (name, was)
            })
            .collect();
            std::env::set_var("HOME", home.path());
            std::env::set_var("XDG_CONFIG_HOME", home.path().join("config"));
            std::env::set_var("XDG_STATE_HOME", home.path().join("xdg-state"));
            // CAD-1922: a `HOME`/XDG-only environment with nothing in it
            // is a first run, which resolves `~/.cadence`. This fixture
            // models an EXISTING installation (ambient local state under
            // `XDG_STATE_HOME`), so it presents that installation's own
            // roots before anything resolves.
            std::fs::create_dir_all(home.path().join("xdg-state").join("cadence")).unwrap();
            std::fs::create_dir_all(home.path().join("config").join("cadence")).unwrap();
            Self { home, saved }
        }
        fn expected_state(&self) -> PathBuf {
            self.home.path().join("xdg-state").join("cadence")
        }
        fn registry_path(&self) -> PathBuf {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap()
                .join("cadence")
                .join("orgs.json")
        }
        pub(in crate::cli) fn write_registry(&self, registry: &Value) {
            let path = self.registry_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, serde_json::to_vec_pretty(registry).unwrap()).unwrap();
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, was) in self.saved.drain(..) {
                match was {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    pub(in crate::cli) fn remote_registry(
        org: &str,
        endpoint: &str,
        org_id: &str,
        selected: bool,
    ) -> Value {
        json!({
            "selected": selected.then(|| json!({"org": org})),
            "connections": [{
                "selection": {"org": org},
                "destination": {"mode": "remote", "endpoint": endpoint, "org_id": org_id},
            }],
        })
    }

    // CAD-2009: independent integration acceptance; preserve these expectations.
    #[test]
    fn cad2009_home_registry_resolution_preserves_declared_root_refusals() {
        const CHILD: &str = "CAD2009_HOME_REGISTRY_CASE";
        const ROOT: &str = "CAD2009_HOME_REGISTRY_FIXTURE";
        const DONE: &str = "CAD2009_HOME_REGISTRY_CHILD_EXERCISED";
        const TEST: &str =
            "cli::org::tests::cad2009_home_registry_resolution_preserves_declared_root_refusals";

        let Some(case) = std::env::var_os(CHILD) else {
            for case in [
                "all-unset",
                "xdg-only",
                "cadence-home-only",
                "invalid-cadence-home-only",
                "invalid-cadence-home-xdg",
                "symlink-cadence-home-only",
                "symlink-cadence-home-xdg",
                "relative-home-xdg",
                "sandbox-invalid-base-xdg",
                "sandbox-no-home-xdg",
                "sandbox-overlap-xdg",
                "sandbox-valid",
                "fresh-exported-xdg",
            ] {
                let fixture = tempfile::tempdir().unwrap();
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                    .env_clear()
                    .env(CHILD, case)
                    .env(ROOT, fixture.path())
                    .env("PATH", "/usr/bin:/bin")
                    .current_dir(fixture.path())
                    .stdin(std::process::Stdio::null());
                let output = cadence_agent::reaper::output(&mut command).unwrap();
                assert!(
                    output.status.success(),
                    "case {case}: stdout={} stderr={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    String::from_utf8_lossy(&output.stdout).contains(DONE),
                    "case {case}: the exact child acceptance did not execute"
                );
            }
            return;
        };
        let case = case.to_str().unwrap();
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        let home = root.join("home");
        let xdg = root.join("xdg");
        let install = root.join("install");
        let base = root.join("sandboxes");
        let sandbox = base.join("marked");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&xdg).unwrap();
        std::fs::create_dir_all(&sandbox).unwrap();
        std::fs::write(sandbox.join(".cadence-sandbox"), br#"{"name":"marked"}"#).unwrap();

        let write_local = |config: &Path, name: &str| {
            std::fs::create_dir_all(config).unwrap();
            let raw = serde_json::to_vec(&json!({
                "selected": {"org": name},
                "connections": [{
                    "selection": {"org": name},
                    "destination": {
                        "mode": "local",
                        "state_dir": root.join("unused-state"),
                        "tracker_dir": root.join("unused-tracker")
                    }
                }]
            }))
            .unwrap();
            let path = config.join("orgs.json");
            std::fs::write(&path, &raw).unwrap();
            (path, raw)
        };
        let mut originals = vec![
            write_local(&install.join("config"), "home-fixture"),
            write_local(&sandbox.join("config"), "sandbox-fixture"),
        ];
        if case != "fresh-exported-xdg" {
            originals.push(write_local(&xdg.join("cadence"), "xdg-fixture"));
        }
        std::os::unix::fs::symlink(&install, root.join("linked-install")).unwrap();

        // All environment changes below occur only in this isolated child.
        if !matches!(
            case,
            "all-unset"
                | "cadence-home-only"
                | "invalid-cadence-home-only"
                | "symlink-cadence-home-only"
        ) {
            std::env::set_var("XDG_CONFIG_HOME", &xdg);
        }
        match case {
            "cadence-home-only" => std::env::set_var("CADENCE_HOME", &install),
            "invalid-cadence-home-only" | "invalid-cadence-home-xdg" => {
                std::env::set_var("CADENCE_HOME", "relative-install");
            }
            "symlink-cadence-home-only" | "symlink-cadence-home-xdg" => {
                std::env::set_var("CADENCE_HOME", root.join("linked-install"));
            }
            "relative-home-xdg" => std::env::set_var("HOME", "relative-home"),
            "sandbox-invalid-base-xdg" => {
                std::env::set_var("CADENCE_PROFILE", "sandbox:marked");
                std::env::set_var("CADENCE_SANDBOX_ROOT", "relative-base");
            }
            "sandbox-no-home-xdg" | "sandbox-overlap-xdg" | "sandbox-valid" => {
                std::env::set_var("CADENCE_PROFILE", "sandbox:marked");
                std::env::set_var("CADENCE_SANDBOX_ROOT", &base);
                if case != "sandbox-no-home-xdg" {
                    std::env::set_var("HOME", &home);
                }
                if case == "sandbox-overlap-xdg" {
                    std::env::set_var("CADENCE_HOME", &sandbox);
                }
            }
            "fresh-exported-xdg" => std::env::set_var("HOME", &home),
            "all-unset" | "xdg-only" => {}
            _ => panic!("unknown case {case}"),
        }

        let expected = match case {
            "xdg-only" => Some((xdg.join("cadence"), Some("xdg-fixture"))),
            "cadence-home-only" => Some((install.join("config"), Some("home-fixture"))),
            "sandbox-valid" => Some((sandbox.join("config"), Some("sandbox-fixture"))),
            "fresh-exported-xdg" => Some((home.join(".cadence/config"), None)),
            _ => None,
        };
        let allowed_login_lock = expected
            .as_ref()
            .filter(|(_, selected)| selected.is_some())
            .map(|(config, _)| config.join("orgs.lock"));
        if let Some((config, selected)) = expected {
            assert_eq!(cadence_agent::home::config_dir().unwrap(), config, "{case}");
            assert_eq!(
                selected_org_readonly().unwrap().as_deref(),
                selected,
                "{case}"
            );
            for (path, _) in &originals {
                assert!(
                    !path.parent().unwrap().join("orgs.lock").exists(),
                    "{case}: read-only selection created a registry lock"
                );
            }
            // Every stored fixture is local: this must not need credentials.
            assert!(saved_login_token(None).unwrap().is_none(), "{case}");
        } else if case == "all-unset" {
            assert!(cadence_agent::home::config_dir().is_err());
            assert!(selected_org_readonly().unwrap().is_none());
            assert!(saved_login_token(None).unwrap().is_none());
        } else {
            assert!(cadence_agent::home::config_dir().is_err(), "{case}");
            assert!(
                selected_org_readonly().is_err(),
                "{case}: reported no saved org"
            );
            assert!(
                saved_login_token(None).is_err(),
                "{case}: silently went anonymous"
            );
        }
        for (path, raw) in originals {
            assert_eq!(std::fs::read(&path).unwrap(), raw, "{case}");
            let lock = path.parent().unwrap().join("orgs.lock");
            if lock.exists() {
                assert_eq!(
                    allowed_login_lock.as_deref(),
                    Some(lock.as_path()),
                    "{case}: saved-login inspection created a lock outside its valid selected root"
                );
            }
        }
        assert!(!home.join(".cadence").exists(), "{case}");
        assert!(!home.join("pm").exists(), "{case}");
        assert!(!root.join("unused-state").exists(), "{case}");
        assert!(!root.join("unused-tracker").exists(), "{case}");
        assert!(!root.join("relative-install").exists(), "{case}");
        assert!(!root.join("relative-home").exists(), "{case}");
        assert!(!root.join("relative-base").exists(), "{case}");
        if case == "fresh-exported-xdg" {
            assert!(!xdg.join("cadence").exists());
        }
        println!("{DONE}");
    }

    #[test]
    fn explicit_remote_org_without_a_registry_is_refused_not_local() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::bare();
        let err = resolve(Some("ghost"), None, None).unwrap_err();
        assert!(err.to_string().contains("org 'ghost'"), "{err}");
        std::env::set_var("CADENCE_ORG", "ghost");
        let err = resolve(None, None, None).unwrap_err();
        assert!(err.to_string().contains("org 'ghost'"), "{err}");
    }

    #[test]
    fn no_org_without_a_registry_still_resolves_local() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        let resolved = resolve(None, None, None).unwrap();
        match resolved {
            Resolved::Local(dir) => assert_eq!(dir, env.expected_state(), "{dir:?}"),
            Resolved::Remote(_) => panic!("ambient local resolved remote"),
        }
    }

    #[test]
    fn explicit_local_is_the_built_in_destination() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        let resolved = resolve(Some("local"), None, None).unwrap();
        match resolved {
            Resolved::Local(dir) => assert_eq!(dir, env.expected_state()),
            Resolved::Remote(_) => panic!("local resolved remote"),
        }
        env.write_registry(&json!({
            "selected": null,
            "connections": [{
                "selection": {"org": "local"},
                "destination": {
                    "mode": "local",
                    "state_dir": "/org/state",
                    "tracker_dir": "/org/tracker",
                },
            }],
        }));
        let resolved = resolve(Some("local"), None, None).unwrap();
        match resolved {
            Resolved::Local(dir) => assert_eq!(dir, PathBuf::from("/org/state")),
            Resolved::Remote(_) => panic!("registered local resolved remote"),
        }
    }

    #[test]
    fn explicit_org_names_a_registered_remote() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        env.write_registry(&remote_registry("acme", EP_A, "ws_alpha", false));
        let resolved = resolve(Some("acme"), None, None).unwrap();
        match resolved {
            Resolved::Remote(target) => {
                assert_eq!(target.org, "acme");
                assert_eq!(target.endpoint, EP_A);
                assert_eq!(target.org_id, "ws_alpha");
            }
            Resolved::Local(_) => panic!("remote org resolved local"),
        }
    }

    #[test]
    fn unknown_explicit_org_in_a_real_registry_is_refused() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        env.write_registry(&remote_registry("acme", EP_A, "ws_alpha", false));
        let err = resolve(Some("ghost"), None, None).unwrap_err();
        assert!(err.to_string().contains("org 'ghost'"), "{err}");
    }

    #[test]
    fn cadence_org_env_names_a_registered_remote() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        env.write_registry(&remote_registry("acme", EP_A, "ws_alpha", false));
        std::env::set_var("CADENCE_ORG", "acme");
        match resolve(None, None, None).unwrap() {
            Resolved::Remote(target) => assert_eq!(target.org, "acme"),
            Resolved::Local(_) => panic!("CADENCE_ORG remote resolved local"),
        }
    }

    #[test]
    fn managed_and_pinned_callers_keep_their_existing_refusals() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        env.write_registry(&remote_registry("acme", EP_A, "ws_alpha", true));
        std::env::set_var("CADENCE_ALIAS", "worker-1");
        assert!(resolve(Some("acme"), None, None).is_err());
        let resolved = resolve(None, None, None).unwrap();
        assert!(matches!(resolved, Resolved::Local(_)));
        std::env::remove_var("CADENCE_ALIAS");
        std::env::set_var("CADENCE_STATE_DIR", "/pinned/state");
        assert!(resolve(Some("acme"), None, None).is_err());
        let resolved = resolve(None, None, None).unwrap();
        match resolved {
            Resolved::Local(dir) => assert_eq!(dir, PathBuf::from("/pinned/state")),
            Resolved::Remote(_) => panic!("pinned caller resolved remote"),
        }
    }

    #[test]
    fn changed_endpoint_or_org_id_replaces_the_stale_binding() {
        // A verified grant for the same slug is authoritative: a recreated
        // or rotated workspace re-points the stored remote instead of
        // requiring a manual registry edit.
        let mut registry = Registry::default();
        apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", false).unwrap();
        apply_record_remote(&mut registry, "alpha", EP_B, "ws_alpha", false).unwrap();
        assert_eq!(registry.connections.len(), 1);
        assert_remote(&registry, "alpha", EP_B, "ws_alpha");

        apply_record_remote(&mut registry, "alpha", EP_A, "ws_other", false).unwrap();
        assert_eq!(registry.connections.len(), 1);
        assert_remote(&registry, "alpha", EP_A, "ws_other");
    }

    #[test]
    fn identical_relogin_is_idempotent() {
        let mut registry = Registry::default();
        apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", false).unwrap();
        apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", false).unwrap();
        assert_eq!(registry.connections.len(), 1);
        assert_remote(&registry, "alpha", EP_A, "ws_alpha");
    }

    #[test]
    fn local_name_collision_is_still_refused() {
        let mut registry = Registry::default();
        registry.connections.push(Connection {
            selection: Selection {
                org: "alpha".into(),
            },
            destination: Destination::Local {
                state_dir: "/local/state".into(),
                tracker_dir: "/local/tracker".into(),
            },
        });
        let error = apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", false)
            .expect_err("local row must not be replaced");
        assert!(
            error.to_string().contains("exists as a local connection"),
            "{error}"
        );
        assert_eq!(registry.connections.len(), 1);
        assert!(matches!(
            registry.connections[0].destination,
            Destination::Local { .. }
        ));
    }

    #[test]
    fn replacement_leaves_other_orgs_and_the_default_untouched() {
        let mut registry = Registry::default();
        apply_record_remote(&mut registry, "beta", EP_B, "ws_beta", true).unwrap();
        apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", false).unwrap();

        let selected =
            apply_record_remote(&mut registry, "alpha", EP_A, "ws_other", false).unwrap();
        assert!(!selected.1, "replacement without --use must not select");
        assert_eq!(
            registry.selected.as_ref().map(|s| s.org.as_str()),
            Some("beta")
        );
        assert_remote(&registry, "alpha", EP_A, "ws_other");
        assert_remote(&registry, "beta", EP_B, "ws_beta");
    }

    #[test]
    fn replacing_the_selected_org_keeps_it_selected_and_use_can_move_it() {
        let mut registry = Registry::default();
        apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", true).unwrap();
        let replaced =
            apply_record_remote(&mut registry, "alpha", EP_A, "ws_other", false).unwrap();
        assert!(replaced.1, "an already selected org stays selected");
        assert_eq!(
            registry.selected.as_ref().map(|s| s.org.as_str()),
            Some("alpha")
        );

        apply_record_remote(&mut registry, "beta", EP_B, "ws_beta", false).unwrap();
        apply_record_remote(&mut registry, "beta", EP_B, "ws_beta2", true).unwrap();
        assert_eq!(
            registry.selected.as_ref().map(|s| s.org.as_str()),
            Some("beta")
        );
    }

    #[test]
    fn registry_write_succeeds_when_the_config_directory_cannot_be_opened_for_read() {
        // persist publishes the registry; a write+execute-only config dir
        // cannot be opened for the fsync, but must not turn a published
        // binding into a failed login that skips the credential save.
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        env.write_registry(&remote_registry("alpha", EP_A, "ws_alpha", true));
        let dir = env.registry_path().parent().unwrap().to_path_buf();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o300)).unwrap();

        let stored = record_remote("alpha", EP_A, "ws_other", false, || Ok(())).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(stored["destination"]["org_id"], "ws_other");
        let raw: Value =
            serde_json::from_slice(&std::fs::read(env.registry_path()).unwrap()).unwrap();
        assert_eq!(raw["connections"][0]["destination"]["org_id"], "ws_other");
    }

    #[test]
    fn failed_credential_commit_restores_the_previous_binding() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        let previous = remote_registry("alpha", EP_A, "ws_alpha", true);
        env.write_registry(&previous);

        let error = record_remote("alpha", EP_A, "ws_other", false, || {
            Err(Error::rejected("credential write refused"))
        })
        .expect_err("a failed credential save must fail the login");
        assert!(
            error.to_string().contains("credential write refused"),
            "{error}"
        );

        let stored: Value =
            serde_json::from_slice(&std::fs::read(env.registry_path()).unwrap()).unwrap();
        assert_eq!(stored, previous, "the previous binding must be restored");
    }

    #[test]
    fn failed_credential_commit_restores_a_compact_registry_verbatim() {
        // read_registry accepts a compact file up to 64 KiB, but write
        // pretty-prints. A compact registry just under the read cap can be
        // impossible to reserialize; rollback must put the original bytes
        // back instead of failing after the replacement was published.
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        let long_id = format!("ws_{}", "a".repeat(196));
        let mut compact = None;
        for endpoint_len in 64_000..=65_400 {
            let registry = Registry {
                selected: Some(Selection {
                    org: "alpha".into(),
                }),
                connections: vec![Connection {
                    selection: Selection {
                        org: "alpha".into(),
                    },
                    destination: Destination::Remote {
                        endpoint: format!("https://{}.test", "a".repeat(endpoint_len)),
                        org_id: long_id.clone(),
                    },
                }],
            };
            let raw = serde_json::to_vec(&registry).unwrap();
            if raw.len() <= 65536 && serde_json::to_vec_pretty(&registry).unwrap().len() > 65536 {
                compact = Some(raw);
                break;
            }
        }
        let compact = compact.expect("no compact registry crosses the pretty-print cap");
        let path = env.registry_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &compact).unwrap();

        let error = record_remote("alpha", EP_A, "ws_new", false, || {
            Err(Error::rejected("credential write refused"))
        })
        .expect_err("a failed credential save must fail the login");
        assert!(
            error.to_string().contains("credential write refused"),
            "{error}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            compact,
            "rollback must restore the compact registry bytes verbatim"
        );
    }

    #[test]
    fn successful_relogin_commits_the_credential_after_replacing() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        env.write_registry(&remote_registry("alpha", EP_A, "ws_alpha", true));
        let mut committed = false;

        let stored = record_remote("alpha", EP_A, "ws_other", false, || {
            committed = true;
            Ok(())
        })
        .unwrap();
        assert!(committed);
        assert_eq!(stored["destination"]["org_id"], "ws_other");
        assert_eq!(stored["selected"], true);

        let raw: Value =
            serde_json::from_slice(&std::fs::read(env.registry_path()).unwrap()).unwrap();
        assert_eq!(raw["connections"][0]["destination"]["org_id"], "ws_other");
        assert_eq!(raw["connections"][0]["destination"]["endpoint"], EP_A);
    }

    fn assert_remote(registry: &Registry, org: &str, endpoint: &str, org_id: &str) {
        let stored = registry
            .connections
            .iter()
            .find(|c| c.selection.org == org)
            .unwrap_or_else(|| panic!("{org} is missing from the registry"));
        match &stored.destination {
            Destination::Remote {
                endpoint: actual_endpoint,
                org_id: actual_org_id,
            } => {
                assert_eq!(actual_endpoint, endpoint);
                assert_eq!(actual_org_id, org_id);
            }
            Destination::Local { .. } => panic!("{org} is not a remote"),
        }
    }

    #[test]
    fn non_https_or_credentialed_endpoint_is_refused() {
        let mut registry = Registry::default();
        assert!(apply_record_remote(
            &mut registry,
            "alpha",
            "http://evil.example/",
            "ws_a",
            false
        )
        .is_err());
        assert!(apply_record_remote(
            &mut registry,
            "alpha",
            "https://user:pass@evil.example/",
            "ws_a",
            false
        )
        .is_err());
        assert!(apply_record_remote(
            &mut registry,
            "alpha",
            "https://evil.example/?next=1",
            "ws_a",
            false
        )
        .is_err());
        assert!(registry.connections.is_empty());
        assert!(registry.selected.is_none());
    }

    fn hosted_message(id: &str, state: &str) -> Value {
        json!({"id": id, "alias": "worker-1", "state": state})
    }

    fn managed_state(env: &EnvGuard) -> PathBuf {
        let dir = env.home.path().join("daemon-state");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("CADENCE_STATE_DIR", &dir);
        std::env::set_var("CADENCE_ALIAS", "worker-1");
        dir
    }

    fn write_login(env: &EnvGuard, subject: &str, issuer: &str) {
        use std::os::unix::fs::PermissionsExt;
        let dir = env.home.path().join("config/cadence/remote-auth");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("cli-acme.json");
        let body = json!({
            "version": "hosted-cadence-auth.v1",
            "organization_id": "ws_binding",
            "organization_slug": "acme",
            "audience": EP_A,
            "issuer": issuer,
            "subject_id": subject,
            "capabilities": ["cli.read", "cli.write"],
            "access_token": format!("hct_{}", "a".repeat(43)),
            "expires_at": 4_000_000_000u64,
        });
        std::fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn bind_hosted(dir: &Path, message: &str) {
        let kickoff = cadence_agent::local_assignment::Kickoff {
            alias: "worker-1",
            issue_id: "CAD-1460",
            assignment_id: "hla_00000000000000000000000000000001",
            worktree: "/tmp/not-a-checkout",
            message_id: message,
            claim_id: "hlc_00000000000000000000000000000001",
            generation: 4,
            organization_id: "ws_binding",
            audience: EP_A,
            subject_id: Some("user_claimant"),
            issuer: Some("https://issuer.example/original"),
        };
        cadence_agent::local_assignment::reporting::store_kickoff_binding(
            dir,
            "acme",
            EP_A,
            &kickoff,
            "hosted kickoff",
        )
        .unwrap();
    }

    fn settle_hosted(dir: &Path, message: &str) {
        let path = dir
            .join("hosted-assignments")
            .join(format!("{message}.bind"));
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["settled"] = json!(true);
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }

    fn serve_show(dir: &Path, messages: Value, wait_ms: i32) -> std::thread::JoinHandle<()> {
        use std::io::{BufRead, Write};
        use std::os::unix::io::AsRawFd;
        use std::os::unix::net::UnixListener;
        let socket = dir.join("cadence.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            let fd = listener.as_raw_fd();
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut pollfd, 1, wait_ms) } <= 0 {
                return;
            }
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = String::new();
            let _ = std::io::BufReader::new(stream.try_clone().unwrap()).read_line(&mut request);
            let accepted = serde_json::from_str::<Value>(&request)
                .ok()
                .is_some_and(|frame| {
                    frame["method"] == "agent_show" && frame["params"]["alias"] == "worker-1"
                });
            let reply = if accepted {
                cadence_agent::proto::ok(json!({
                    "agent": {"alias": "worker-1"},
                    "messages": messages,
                }))
            } else {
                json!({"ok": false, "error": {"kind": "rejected", "message": "unexpected agent_show"}})
            };
            let _ = writeln!(stream, "{reply}");
        })
    }

    fn assert_refused(dir: &Path, messages: Value, org: &str) {
        let server = serve_show(dir, messages, 3_000);
        let err = resolve(Some(org), None, None).unwrap_err();
        server.join().unwrap();
        let _ = std::fs::remove_file(dir.join("cadence.sock"));
        assert_eq!(err.kind(), "rejected", "{err}");
        let text = err.to_string();
        assert!(!text.to_ascii_lowercase().contains("unset"), "{text}");
        assert!(!text.contains("unexpected"), "{text}");
        assert!(!text.contains("Daemon is not reachable"), "{text}");
    }

    #[test]
    fn managed_running_binding_uses_that_hosted_org() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        let dir = managed_state(&env);
        std::env::set_var("CADENCE_ORG", "other");
        std::env::set_var("CADENCE_PM_DIR", "/inherited/pm");
        env.write_registry(&remote_registry("acme", EP_B, "ws_registry", true));
        write_login(&env, "user_claimant", "https://issuer.example/original");
        bind_hosted(&dir, "msg-live");
        let server = serve_show(
            &dir,
            json!([
                hosted_message("msg-old", "completed"),
                hosted_message("msg-wait", "queued"),
                hosted_message("msg-live", "running"),
            ]),
            3_000,
        );
        let resolved = resolve(Some("acme"), None, Some(PathBuf::from("/inherited/pm"))).unwrap();
        server.join().unwrap();
        let _ = std::fs::remove_file(dir.join("cadence.sock"));
        match resolved {
            Resolved::Remote(target) => {
                assert_eq!(target.org, "acme");
                assert_eq!(target.endpoint, EP_A);
                assert_eq!(target.org_id, "ws_binding");
            }
            Resolved::Local(path) => panic!("live binding resolved local {path:?}"),
        }
        match resolve(None, None, Some(PathBuf::from("/inherited/pm"))).unwrap() {
            Resolved::Local(path) => assert_eq!(path, dir),
            Resolved::Remote(target) => panic!("omitted --org resolved {}", target.endpoint),
        }
    }

    #[test]
    fn managed_wrong_unbound_ended_and_ambiguous_selectors_are_refused() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        let dir = managed_state(&env);
        env.write_registry(&remote_registry("other", EP_B, "ws_other", true));
        bind_hosted(&dir, "msg-live");
        bind_hosted(&dir, "msg-done");
        assert_refused(
            &dir,
            json!([hosted_message("msg-live", "running")]),
            "other",
        );
        assert_refused(
            &dir,
            json!([
                hosted_message("msg-gap", "running"),
                hosted_message("msg-done", "completed"),
            ]),
            "acme",
        );
        assert_refused(
            &dir,
            json!([hosted_message("msg-done", "completed")]),
            "acme",
        );
        assert_refused(&dir, json!([hosted_message("msg-live", "queued")]), "acme");
        assert_refused(&dir, json!([hosted_message("msg-live", "unknown")]), "acme");
        assert_refused(
            &dir,
            json!([
                hosted_message("msg-live", "running"),
                hosted_message("msg-other", "running"),
            ]),
            "acme",
        );
        assert_refused(&dir, json!([]), "acme");
        settle_hosted(&dir, "msg-live");
        assert_refused(&dir, json!([hosted_message("msg-live", "running")]), "acme");
    }

    #[test]
    fn managed_explicit_state_and_foreign_tracker_still_conflict() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        let dir = managed_state(&env);
        bind_hosted(&dir, "msg-live");
        let err = resolve(Some("acme"), Some(dir.clone()), None).unwrap_err();
        assert!(err.to_string().contains("conflicts"), "{err}");
        std::env::set_var("CADENCE_PM_DIR", "/inherited/pm");
        let err = resolve(Some("acme"), None, Some(PathBuf::from("/other/pm"))).unwrap_err();
        assert!(err.to_string().contains("conflicts"), "{err}");
    }

    #[test]
    fn managed_update_does_not_follow_the_live_hosted_org() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        let dir = managed_state(&env);
        bind_hosted(&dir, "msg-live");
        write_login(&env, "user_claimant", "https://issuer.example/original");
        let err = resolve_update(Some("acme"), None, None).unwrap_err();
        assert_eq!(err.kind(), "rejected", "{err}");
        let server = serve_show(&dir, json!([hosted_message("msg-live", "running")]), 3_000);
        let resolved = resolve(Some("acme"), None, None).unwrap();
        server.join().unwrap();
        assert!(
            matches!(resolved, Resolved::Remote(target) if target.org == "acme"
            && target.endpoint == EP_A
            && target.org_id == "ws_binding")
        );
    }

    #[test]
    fn managed_caller_still_cannot_switch_org() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        env.write_registry(&remote_registry("acme", EP_A, "ws_alpha", false));
        std::env::set_var("CADENCE_ALIAS", "worker-1");
        let err = run(OrgAction::Switch {
            name: "acme".into(),
        })
        .unwrap_err();
        assert_eq!(err.kind(), "rejected", "{err}");
        let registry: Value =
            serde_json::from_slice(&std::fs::read(env.registry_path()).unwrap()).unwrap();
        assert!(registry["selected"].is_null());
    }

    #[test]
    fn managed_binding_without_member_or_another_login_cannot_retarget() {
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        let dir = managed_state(&env);
        bind_hosted(&dir, "msg-live");
        let path = dir.join("hosted-assignments/msg-live.bind");
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("subject_id");
        value.as_object_mut().unwrap().remove("issuer");
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        write_login(&env, "user_claimant", "https://issuer.example/original");
        assert_refused(&dir, json!([hosted_message("msg-live", "running")]), "acme");
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["subject_id"] = json!("user_claimant");
        value["issuer"] = json!("https://issuer.example/original");
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        write_login(&env, "user_other", "https://issuer.example/later");
        let server = serve_show(&dir, json!([hosted_message("msg-live", "running")]), 3_000);
        let err = resolve(Some("acme"), None, None).unwrap_err();
        server.join().unwrap();
        assert_eq!(err.kind(), "rejected", "{err}");
    }
}
