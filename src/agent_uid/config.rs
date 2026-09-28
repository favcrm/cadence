//! ADR 0007 Stage B: a private per-state-dir opt-in for the agent UID.
//! Absence preserves the legacy same-UID daemon. A malformed or stale
//! record refuses startup rather than silently falling back to it.

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use serde::Deserialize;

use super::{LiveHost, View, AGENT_USER, SHARED_GROUP};
use crate::error::{Error, Result};

pub const RECORD: &str = "agent-uid.json";
pub const SHARED_SOCKET: &str = "/var/lib/cadence/cadence.sock";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    uid: u32,
}

/// Read a root/private state record only when it exists. T7 may write
/// it after T1 provisioning; T3 itself never creates it.
pub fn configured_uid(state_dir: &Path) -> Result<Option<u32>> {
    let path = state_dir.join(RECORD);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::rejected(format!(
                "agent UID record refused: {error}"
            )))
        }
    };
    let meta = file.metadata()?;
    let euid = unsafe { libc::geteuid() };
    if !meta.is_file() || meta.uid() != euid || meta.mode() & 0o077 != 0 || meta.len() > 4096 {
        return Err(Error::rejected(
            "agent UID record owner, mode or size refused",
        ));
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(Error::rejected("agent UID record too large"));
    }
    let record: Record = serde_json::from_slice(&bytes)
        .map_err(|_| Error::rejected("agent UID record is malformed"))?;
    if record.uid == 0 || record.uid == euid {
        return Err(Error::rejected(
            "agent UID record names root or the daemon UID",
        ));
    }
    let agent = LiveHost::new()
        .user(AGENT_USER)?
        .ok_or_else(|| Error::rejected("agent UID record exists but cadence-agent is missing"))?;
    if agent.uid != record.uid {
        return Err(Error::rejected(
            "agent UID record no longer matches cadence-agent",
        ));
    }
    Ok(Some(record.uid))
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
}
