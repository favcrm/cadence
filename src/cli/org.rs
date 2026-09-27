//! CAD-657: user defaults are client configuration, never running-team state.
//! One locked snapshot is resolved per CLI invocation. Remote records remain
//! inert until CAD-539 supplies an authenticated transport.
use super::*;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

#[derive(Subcommand)]
pub(crate) enum OrgAction {
    /// Show registered destinations as JSON, without credentials.
    List,
    /// Register a connection; does not create a daemon, tracker or membership.
    Add {
        /// Local label, not proof of organization membership.
        name: String,
        /// Absolute local runtime directory. Requires --tracker-dir.
        #[arg(
            long = "local-state-dir",
            requires = "tracker_dir",
            conflicts_with = "endpoint"
        )]
        local_state_dir: Option<PathBuf>,
        #[arg(long, requires = "local_state_dir", conflicts_with = "endpoint")]
        tracker_dir: Option<PathBuf>,
        /// HTTPS remote destination. Transport/auth are not yet implemented.
        #[arg(long, requires = "org_id", conflicts_with = "local_state_dir")]
        endpoint: Option<String>,
        /// Cloud-issued organization identity, separate from the local label.
        #[arg(long, requires = "endpoint")]
        org_id: Option<String>,
    },
    /// Change this user's default only; existing teams retain their bindings.
    Switch { name: String },
    /// Inspect a registered connection, or the selected default.
    Inspect { name: Option<String> },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Selection {
    org: String,
    connection: String,
}

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
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(path) => PathBuf::from(path),
            None => PathBuf::from(
                std::env::var_os("HOME").ok_or_else(|| Error::rejected("HOME is not set"))?,
            )
            .join(".config"),
        };
        absolute(&base)?;
        Ok(base.join("cadence").join("orgs.json"))
    }
    fn open() -> Result<Self> {
        let path = Self::path()?;
        let dir = path.parent().expect("registry parent");
        std::fs::create_dir_all(dir)?;
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join("orgs.lock"))?;
        if !lock.metadata()?.is_file() {
            return Err(Error::rejected("org registry lock must be a regular file"));
        }
        // SAFETY: flock acts only on the descriptor owned by this guard.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self { path, _lock: lock })
    }
    fn read(&self) -> Result<Registry> {
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Registry::default()),
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
            cadence_agent::proto::identifier(&c.selection.connection, "connection")?;
            validate_destination(&c.destination)?;
            if registry.connections[..i]
                .iter()
                .any(|p| p.selection == c.selection)
            {
                return Err(Error::rejected("duplicate org connection in registry"));
            }
        }
        if let Some(selected) = &registry.selected {
            if !registry
                .connections
                .iter()
                .any(|c| &c.selection == selected)
            {
                return Err(Error::rejected("selected org connection is missing"));
            }
        }
        Ok(registry)
    }
    fn write(&self, registry: &Registry) -> Result<()> {
        let dir = self.path.parent().expect("registry parent");
        let mut temp = tempfile::NamedTempFile::new_in(dir)?;
        let raw =
            serde_json::to_vec_pretty(registry).map_err(|e| Error::internal(e.to_string()))?;
        if raw.len() > 65536 {
            return Err(Error::rejected(
                "org registry exceeds 64 KiB; no configuration changed",
            ));
        }
        temp.write_all(&raw)?;
        temp.as_file().sync_all()?;
        temp.persist(&self.path)
            .map_err(|e| Error::internal(e.to_string()))?;
        File::open(dir)?.sync_all()?;
        Ok(())
    }
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
                return Err(Error::rejected("remote endpoint must not contain credentials, query, fragment or control characters"));
            }
            cadence_agent::proto::identifier(org_id, "org-id")?;
        }
    }
    Ok(())
}
fn select(
    registry: &Registry,
    org: Option<&str>,
    connection: Option<&str>,
) -> Result<Option<Connection>> {
    let Some(org) = org.or_else(|| registry.selected.as_ref().map(|s| s.org.as_str())) else {
        if connection.is_some() {
            return Err(Error::rejected(
                "--connection requires an org or selected default",
            ));
        }
        return Ok(None);
    };
    let candidates: Vec<_> = registry
        .connections
        .iter()
        .filter(|c| c.selection.org == org)
        .collect();
    let name = connection.or_else(|| {
        registry
            .selected
            .as_ref()
            .filter(|s| s.org == org)
            .map(|s| s.connection.as_str())
    });
    if let Some(name) = name {
        return candidates
            .into_iter()
            .find(|c| c.selection.connection == name)
            .cloned()
            .map(Some)
            .ok_or_else(|| Error::rejected("unknown org connection"));
    }
    match candidates.as_slice() {
        [only] => Ok(Some((**only).clone())),
        [] => Err(Error::rejected("unknown org")),
        _ => Err(Error::rejected(
            "org has multiple connections; pass --connection",
        )),
    }
}
fn view(c: &Connection, selected: bool) -> Value {
    json!({"org": c.selection.org, "connection": c.selection.connection, "selected": selected,
        "destination": c.destination, "login_status": match c.destination { Destination::Local { .. } => "local_peer_authority", Destination::Remote { .. } => "not_connected" }})
}
pub(super) fn run(
    action: OrgAction,
    proof_state: &Path,
    connection: Option<String>,
) -> Result<i32> {
    if matches!(action, OrgAction::Add { .. } | OrgAction::Switch { .. }) {
        // Current caller's peer proof, never authority from the new destination.
        cadence_agent::rollout::require_operator(proof_state, "cadence org configuration")?;
    }
    let file = RegistryFile::open()?;
    let mut registry = file.read()?;
    let out = match action {
        OrgAction::List => {
            json!({"connections": registry.connections.iter().map(|c| view(c, registry.selected.as_ref() == Some(&c.selection))).collect::<Vec<_>>() })
        }
        OrgAction::Add {
            name,
            local_state_dir,
            tracker_dir,
            endpoint,
            org_id,
        } => {
            let selection = Selection {
                org: cadence_agent::proto::identifier(&name, "org")?,
                connection: cadence_agent::proto::identifier(
                    connection.as_deref().unwrap_or("default"),
                    "connection",
                )?,
            };
            if registry
                .connections
                .iter()
                .any(|c| c.selection == selection)
            {
                return Err(Error::rejected(
                    "org connection already exists; no configuration changed",
                ));
            }
            let destination = match (local_state_dir, tracker_dir, endpoint, org_id) {
                (Some(state_dir), Some(tracker_dir), None, None) => Destination::Local { state_dir, tracker_dir },
                (None, None, Some(endpoint), Some(org_id)) => Destination::Remote { endpoint, org_id },
                _ => return Err(Error::rejected("specify local --local-state-dir and --tracker-dir, or remote --endpoint and --org-id")),
            };
            validate_destination(&destination)?;
            let c = Connection {
                selection,
                destination,
            };
            let out = view(&c, false);
            registry.connections.push(c);
            file.write(&registry)?;
            out
        }
        OrgAction::Switch { name } => {
            let c = select(&registry, Some(&name), connection.as_deref())?.expect("explicit org");
            registry.selected = Some(c.selection.clone());
            file.write(&registry)?;
            view(&c, true)
        }
        OrgAction::Inspect { name } => {
            let c = select(&registry, name.as_deref(), connection.as_deref())?
                .ok_or_else(|| Error::rejected("no org selected"))?;
            view(&c, registry.selected.as_ref() == Some(&c.selection))
        }
    };
    print_json(&out);
    Ok(0)
}

/// Ambient instance bindings win over a user default. Explicit org selection
/// conflicts with them rather than silently mixing state and tracker roots.
pub(super) fn resolve(
    org: Option<&str>,
    connection: Option<&str>,
    state: Option<PathBuf>,
) -> Result<PathBuf> {
    if std::env::var_os("CADENCE_ALIAS").is_some()
        && std::env::var_os("CADENCE_STATE_DIR").is_none()
    {
        return Err(Error::rejected(
            "managed caller is missing its pinned CADENCE_STATE_DIR; refusing user defaults",
        ));
    }
    let pinned = state.is_some()
        || [
            "CADENCE_STATE_DIR",
            "CADENCE_PM_DIR",
            "CADENCE_HOME",
            "CADENCE_PROFILE",
        ]
        .iter()
        .any(|name| std::env::var_os(name).is_some());
    if pinned {
        if org.is_some() || connection.is_some() {
            return Err(Error::rejected("--org/--connection conflict with an explicit or inherited local instance binding; use a clean operator shell"));
        }
        return state.map(Ok).unwrap_or_else(client::state_dir);
    }
    if org.is_none() && connection.is_none() && !RegistryFile::path()?.exists() {
        return client::state_dir();
    }
    let file = RegistryFile::open()?;
    let registry = file.read()?;
    let Some(c) = select(&registry, org, connection)? else {
        return client::state_dir();
    };
    match c.destination {
        Destination::Local {
            state_dir,
            tracker_dir,
        } => {
            std::env::set_var("CADENCE_STATE_DIR", &state_dir);
            std::env::set_var("CADENCE_PM_DIR", tracker_dir);
            // Printed on stderr so machine-readable stdout remains compatible.
            eprintln!(
                "cadence org={} connection={} local={}",
                c.selection.org,
                c.selection.connection,
                state_dir.display()
            );
            Ok(state_dir)
        }
        Destination::Remote { .. } => Err(Error::rejected(
            "selected remote transport is not implemented (CAD-539); refusing local fallback",
        )),
    }
}
