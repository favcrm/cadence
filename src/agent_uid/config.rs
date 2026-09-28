//! ADR 0007 Stage B: a private per-state-dir opt-in for the agent UID.
//! Absence preserves the legacy same-UID daemon. A malformed or stale
//! record refuses startup rather than silently falling back to it.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use serde::Deserialize;

use super::{LiveHost, View, AGENT_USER, SHARED_GROUP};
use crate::error::{Error, Result};

pub const RECORD: &str = "agent-uid.json";
pub const MODE_MARKER: &str = "agent-uid-mode.json";
pub const SHARED_SOCKET: &str = "/var/lib/cadence/cadence.sock";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    uid: u32,
}

fn read_private_uid(state_dir: &Path, name: &str, label: &str) -> Result<Option<u32>> {
    let path = state_dir.join(name);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::rejected(format!("{label} refused: {error}"))),
    };
    let meta = file.metadata()?;
    let euid = unsafe { libc::geteuid() };
    if !meta.is_file() || meta.uid() != euid || meta.mode() & 0o077 != 0 || meta.len() > 4096 {
        return Err(Error::rejected(format!(
            "{label} owner, mode or size refused"
        )));
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(Error::rejected(format!("{label} too large")));
    }
    let record: Record = serde_json::from_slice(&bytes)
        .map_err(|_| Error::rejected(format!("{label} is malformed")))?;
    if record.uid == 0 || record.uid == euid {
        return Err(Error::rejected(format!(
            "{label} names root or the daemon UID"
        )));
    }
    Ok(Some(record.uid))
}

/// Read a root/private state record only when it exists. T7 may write
/// it after T1 provisioning; T3 itself never creates it.
pub fn configured_uid(state_dir: &Path) -> Result<Option<u32>> {
    let Some(uid) = read_private_uid(state_dir, RECORD, "agent UID record")? else {
        return Ok(None);
    };
    let agent = LiveHost::new()
        .user(AGENT_USER)?
        .ok_or_else(|| Error::rejected("agent UID record exists but cadence-agent is missing"))?;
    if agent.uid != uid {
        return Err(Error::rejected(
            "agent UID record no longer matches cadence-agent",
        ));
    }
    Ok(Some(uid))
}

/// A previous split-mode boot is permanent evidence until an explicit
/// operator deprovision. Losing the mutable config record or daemon
/// socket must never silently restore same-UID authority.
pub fn mode_marker_uid(state_dir: &Path) -> Result<Option<u32>> {
    read_private_uid(state_dir, MODE_MARKER, "agent UID mode marker")
}

pub fn require_private_state_dir(state_dir: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(state_dir)?;
    let euid = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.uid() != euid || meta.mode() & 0o022 != 0 {
        return Err(Error::rejected(
            "agent UID state directory must be owned by the operator and not group/other writable",
        ));
    }
    Ok(())
}

/// Called with the daemon singleton held, before shared socket admission.
/// The marker is private and durable; an existing marker pins the exact
/// UID and forbids an implicit downgrade or changed-UID restart.
pub fn ensure_mode_marker(state_dir: &Path, configured: Option<u32>) -> Result<()> {
    let marked = mode_marker_uid(state_dir)?;
    if configured.is_some() || marked.is_some() {
        require_private_state_dir(state_dir)?;
    }
    if let Some(marked) = marked {
        if Some(marked) != configured {
            return Err(Error::rejected(
                "agent UID mode marker requires its original configured UID",
            ));
        }
        return Ok(());
    }
    let Some(uid) = configured else {
        return Ok(());
    };
    let path = state_dir.join(MODE_MARKER);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| {
            Error::rejected(format!("agent UID mode marker create refused: {error}"))
        })?;
    write!(file, "{{\"uid\":{uid}}}")?;
    file.sync_all()?;
    std::fs::File::open(state_dir)?.sync_all()?;
    if mode_marker_uid(state_dir)? != Some(uid) {
        return Err(Error::rejected("agent UID mode marker did not persist"));
    }
    Ok(())
}

/// T1's fixed shared group, resolved at bind time rather than trusting
/// a caller-supplied gid. No host mutation occurs here.
pub fn shared_gid() -> Result<u32> {
    LiveHost::new()
        .group(SHARED_GROUP)?
        .map(|group| group.gid)
        .ok_or_else(|| Error::rejected("cadence shared group is missing"))
}

pub fn shared_socket_path() -> &'static Path {
    Path::new(SHARED_SOCKET)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn record_is_default_off_and_rejects_symlink_and_loose_mode() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(configured_uid(dir.path()).unwrap(), None);
        let path = dir.path().join(RECORD);
        let target = dir.path().join("target");
        std::fs::write(&target, br#"{"uid":2000}"#).unwrap();
        symlink(&target, &path).unwrap();
        assert!(configured_uid(dir.path()).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, br#"{"uid":2000}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(configured_uid(dir.path()).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, br#"{"uid":0}"#).unwrap();
        assert!(configured_uid(dir.path()).is_err(), "root UID must refuse");
    }

    #[test]
    fn marker_is_durable_private_and_forbids_silent_downgrade() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(mode_marker_uid(dir.path()).unwrap(), None);
        ensure_mode_marker(dir.path(), None).unwrap();
        assert_eq!(mode_marker_uid(dir.path()).unwrap(), None);
        ensure_mode_marker(dir.path(), Some(2200)).unwrap();
        let marker = dir.path().join(MODE_MARKER);
        let meta = std::fs::symlink_metadata(&marker).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.uid(), unsafe { libc::geteuid() });
        assert_eq!(meta.mode() & 0o777, 0o600);
        assert_eq!(mode_marker_uid(dir.path()).unwrap(), Some(2200));
        ensure_mode_marker(dir.path(), Some(2200)).unwrap();
        assert!(ensure_mode_marker(dir.path(), None).is_err());
        assert!(ensure_mode_marker(dir.path(), Some(3300)).is_err());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(ensure_mode_marker(dir.path(), Some(2200)).is_err());
    }

    #[test]
    fn malformed_or_forged_marker_never_becomes_mode_authority() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(MODE_MARKER);
        let target = dir.path().join("target");
        std::fs::write(&target, br#"{"uid":2200}"#).unwrap();
        symlink(&target, &marker).unwrap();
        assert!(mode_marker_uid(dir.path()).is_err());
        std::fs::remove_file(&marker).unwrap();
        std::fs::write(&marker, br#"{"uid":2200,"extra":true}"#).unwrap();
        assert!(mode_marker_uid(dir.path()).is_err());
        std::fs::write(&marker, br#"{"uid":2200}"#).unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(mode_marker_uid(dir.path()).is_err());
    }
}
