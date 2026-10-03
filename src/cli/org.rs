//! CAD-1019 slice 1a: a slim org registry — user preference, never authority.
//!
//! `orgs.json` under `$XDG_CONFIG_HOME/cadence` (or `~/.config/cadence`)
//! holds the operator's org/connection selection. It is a preference file:
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
//! - A managed caller (`CADENCE_ALIAS`) keeps its inherited binding:
//!   `--org`/registry defaults are refused, never silently merged.
//! - A `switch` mid-command cannot move it: each invocation resolves its
//!   destination once at startup.
//! - `Remote` rows persist the issuer-verified endpoint and org id but are
//!   refused by `resolve` — no transport in this slice, no local fallback.
//!
//! Adapted from PR #415 (`5e5a4c6a26ecc68de1719a77320868c04c20219c`) — the
//! operator-proof writes, `add`/`remove` verbs and per-connection selection
//! are removed per the CAD-1019 PM direction; the on-disk `orgs.json` shape
//! is unchanged so a later CAD-657 merge does not fork the format.

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
    /// Exclusive flock: every read-modify-write is one locked snapshot, so a
    /// `login` racing a `switch` cannot interleave a stale default.
    fn open() -> Result<Self> {
        let path = Self::path()?;
        let dir = path.parent().expect("registry parent");
        std::fs::create_dir_all(dir)?;
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
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
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
        .ok_or_else(|| Error::rejected("unknown org"))
}

fn view(c: &Connection, selected: bool) -> Value {
    json!({
        "org": c.selection.org,
        "selected": selected,
        "destination": c.destination,
        "login_status": match c.destination {
            Destination::Local { .. } => "local_peer_authority",
            Destination::Remote { .. } => "not_connected",
        }
    })
}

/// `cadence org …` — no operator proof: a preference, never authority.
pub(super) fn run(action: OrgAction) -> Result<i32> {
    // A managed caller may read but never change the saved default — a UX
    // guard, not a security claim.
    let alias_bound = std::env::var_os("CADENCE_ALIAS").is_some();
    // `switch local` materializes the first-class connection before the
    // shared read, so `ensure_local`'s write lock never nests under this one.
    if matches!(&action, OrgAction::Switch { name } if name == LOCAL_ORG) {
        if alias_bound {
            return Err(Error::rejected(
                "a managed caller cannot change the saved org default — \
                 run `cadence org switch` from the operator's shell",
            ));
        }
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
            if alias_bound {
                return Err(Error::rejected(
                    "a managed caller cannot change the saved org default — \
                     run `cadence org switch` from the operator's shell",
                ));
            }
            let c = select(&registry, Some(&name))?.expect("explicit org");
            registry.selected = Some(c.selection.clone());
            file.write(&registry)?;
            view(&c, true)
        }
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
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
/// `endpoint`/`org_id` come only from the verified grant. A stored remote is
/// never re-pointed, a `local` row is never overwritten, and the new org is
/// selected only when there is no default or `use_default`.
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
        .iter()
        .find(|c| c.selection == selection)
    {
        // Re-login must re-point at the same endpoint + org id — a changed
        // slug or forged audience refuses, never silently moves the org.
        match &existing.destination {
            Destination::Remote {
                endpoint: ep,
                org_id: oid,
            } if ep == endpoint && oid == org_id => {}
            Destination::Remote { .. } => {
                return Err(Error::rejected(
                    "a different endpoint or org id is already stored for this org; \
                     remove it first",
                ));
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
    let selected = if use_default || registry.selected.is_none() {
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

/// File-locked wrapper over `apply_record_remote`; returns the stored view.
pub(super) fn record_remote(
    org_name: &str,
    endpoint: &str,
    org_id: &str,
    use_default: bool,
) -> Result<Value> {
    let file = RegistryFile::open()?;
    let mut registry = file.read()?;
    let (connection, selected) =
        apply_record_remote(&mut registry, org_name, endpoint, org_id, use_default)?;
    file.write(&registry)?;
    Ok(view(&connection, selected))
}

/// Resolve this invocation's daemon state dir once. `org` is `--org`; a
/// managed caller keeps its ambient binding and never consults the
/// registry. `state`/`tracker` are explicit pins — either wins over the
/// registry and refuses `--org`. Returns the state dir; a local-org
/// selection also exports `CADENCE_PM_DIR` so the tracker lands on the
/// org's root. Never resolves the tracker for a managed, pinned or
/// ambient caller — those never needed it (the CAD-313 `ui login`
/// residual runs with no `HOME`).
pub(super) fn resolve(
    org: Option<&str>,
    state: Option<PathBuf>,
    tracker: Option<PathBuf>,
) -> Result<PathBuf> {
    // A managed caller's inherited binding always wins over a saved
    // preference — it cannot be retargeted by a default the operator
    // changed mid-run, and it never reads the registry at all.
    if std::env::var_os("CADENCE_ALIAS").is_some() {
        if org.is_some() {
            return Err(Error::rejected(
                "managed callers cannot override their destination with --org",
            ));
        }
        return state
            .map(Ok)
            .unwrap_or_else(cadence_agent::client::state_dir);
    }
    // An explicit flag or inherited binding is a pin; it conflicts with
    // `--org` rather than mixing roots.
    let pinned = state.is_some()
        || tracker.is_some()
        || [
            "CADENCE_STATE_DIR",
            "CADENCE_PM_DIR",
            "CADENCE_HOME",
            "CADENCE_PROFILE",
        ]
        .iter()
        .any(|name| std::env::var_os(name).is_some());
    if pinned {
        if org.is_some() {
            return Err(Error::rejected(
                "--org conflicts with an explicit or inherited local instance binding; \
                 use a clean operator shell",
            ));
        }
        return state
            .map(Ok)
            .unwrap_or_else(cadence_agent::client::state_dir);
    }
    let org_env = std::env::var("CADENCE_ORG").ok();
    let org = org.or(org_env.as_deref());
    // No org anywhere and no registry → ambient local; the registry never
    // has to exist for standalone local use.
    let file = match RegistryFile::path() {
        Ok(p) if p.exists() => Some(RegistryFile::open()?),
        _ => None,
    };
    let selection = match &file {
        Some(file) => {
            let registry = file.read()?;
            select(&registry, org)?
        }
        None => None,
    };
    let Some(conn) = selection else {
        return cadence_agent::client::state_dir();
    };
    match conn.destination {
        Destination::Local {
            state_dir,
            tracker_dir,
        } => {
            std::env::set_var("CADENCE_STATE_DIR", &state_dir);
            std::env::set_var("CADENCE_PM_DIR", &tracker_dir);
            eprintln!(
                "cadence org={} local={}",
                conn.selection.org,
                state_dir.display()
            );
            Ok(state_dir)
        }
        Destination::Remote { endpoint, org_id } => Err(Error::rejected(format!(
            "org '{}' selects remote endpoint {endpoint} (org {org_id}) but remote \
             transport is not configured in this build — refusing local fallback",
            conn.selection.org
        ))),
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
mod tests {
    use super::*;

    const EP_A: &str = "https://alpha.cadencecloud.app";
    const EP_B: &str = "https://beta.cadencecloud.app";

    fn local_registry() -> Registry {
        Registry {
            selected: None,
            connections: vec![Connection {
                selection: Selection {
                    org: LOCAL_ORG.to_string(),
                },
                destination: Destination::Local {
                    state_dir: PathBuf::from("/state"),
                    tracker_dir: PathBuf::from("/tracker"),
                },
            }],
        }
    }

    #[test]
    fn first_login_selects_the_default() {
        let mut registry = Registry::default();
        let (_, selected) =
            apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", false).unwrap();
        assert!(selected);
        assert_eq!(registry.selected.as_ref().unwrap().org, "alpha");
    }

    #[test]
    fn second_org_login_leaves_the_default_unchanged() {
        // I5 (`cli_second_login_keeps_default`): workspace B login adds
        // an org and leaves the default untouched without `--use`.
        let mut registry = Registry::default();
        apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", false).unwrap();
        let (_, selected) =
            apply_record_remote(&mut registry, "beta", EP_B, "ws_beta", false).unwrap();
        assert!(!selected);
        assert_eq!(registry.selected.as_ref().unwrap().org, "alpha");
        assert_eq!(registry.connections.len(), 2);
    }

    #[test]
    fn use_flag_moves_the_default() {
        let mut registry = Registry::default();
        apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", false).unwrap();
        let (_, selected) =
            apply_record_remote(&mut registry, "beta", EP_B, "ws_beta", true).unwrap();
        assert!(selected);
        assert_eq!(registry.selected.as_ref().unwrap().org, "beta");
    }

    #[test]
    fn relogin_with_the_same_endpoint_and_org_id_is_idempotent() {
        let mut registry = Registry::default();
        apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", false).unwrap();
        let (_, selected) =
            apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", false).unwrap();
        assert!(selected);
        assert_eq!(registry.connections.len(), 1);
    }

    #[test]
    fn changed_endpoint_or_org_id_is_refused_never_moved() {
        // A changed slug or forged audience refuses instead of silently
        // re-pointing the stored remote at another workspace.
        let mut registry = Registry::default();
        apply_record_remote(&mut registry, "alpha", EP_A, "ws_alpha", false).unwrap();
        assert!(apply_record_remote(&mut registry, "alpha", EP_B, "ws_alpha", false).is_err());
        assert!(apply_record_remote(&mut registry, "alpha", EP_A, "ws_other", false).is_err());
        let stored = registry
            .connections
            .iter()
            .find(|c| c.selection.org == "alpha")
            .unwrap();
        match &stored.destination {
            Destination::Remote { endpoint, org_id } => {
                assert_eq!(endpoint, EP_A);
                assert_eq!(org_id, "ws_alpha");
            }
            Destination::Local { .. } => panic!("stored remote changed shape"),
        }
    }

    #[test]
    fn login_over_a_local_name_is_refused() {
        let mut registry = local_registry();
        assert!(apply_record_remote(&mut registry, "local", EP_A, "ws_alpha", false).is_err());
        assert_eq!(registry.connections.len(), 1);
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

    #[test]
    fn invalid_org_name_is_refused() {
        let mut registry = Registry::default();
        assert!(apply_record_remote(&mut registry, "Alpha", EP_A, "ws_a", false).is_err());
        assert!(registry.connections.is_empty());
    }
}
