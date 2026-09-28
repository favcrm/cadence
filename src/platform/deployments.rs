//! Image-owned assertions about provider deployments, never remote discovery.
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use crate::error::{Error, Result};
use serde::Deserialize;

const CAP: u64 = 16 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentMetadata {
    schema: u32,
    providers: Vec<ProviderDeployment>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderDeployment {
    provider: String,
    origin: String,
    manifest_pin: String,
    #[serde(default)]
    image_hosts: Vec<String>,
}

fn refused() -> Error {
    Error::rejected("trusted provider deployment metadata refused")
}

fn valid_authority(authority: &str) -> bool {
    let port_valid = |port: &str| {
        !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok()
    };
    if let Some(ipv6) = authority.strip_prefix('[') {
        let Some((host, suffix)) = ipv6.split_once(']') else {
            return false;
        };
        return host.parse::<std::net::Ipv6Addr>().is_ok()
            && (suffix.is_empty() || suffix.strip_prefix(':').is_some_and(port_valid));
    }
    let (host, port) = authority
        .split_once(':')
        .map_or((authority, None), |(host, port)| (host, Some(port)));
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && port.is_none_or(port_valid)
}

impl DeploymentMetadata {
    /// Programmatic embedding composition only; not a company config field.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() as u64 > CAP {
            return Err(refused());
        }
        let metadata: Self = serde_json::from_slice(bytes).map_err(|_| refused())?;
        let mut seen = std::collections::BTreeSet::new();
        if metadata.schema != 1 || metadata.providers.len() > 32 {
            return Err(refused());
        }
        for entry in &metadata.providers {
            let authority = entry
                .origin
                .strip_prefix("http://")
                .or_else(|| entry.origin.strip_prefix("https://"))
                .ok_or_else(refused)?;
            if !seen.insert(&entry.provider)
                || (entry.provider != crate::platform::agenticos_external::PLATFORM
                    && crate::proto::identifier(&entry.provider, "Provider").is_err())
                || !valid_authority(authority)
                || entry.manifest_pin.is_empty()
                || entry.manifest_pin.len() > 256
                || entry.manifest_pin.chars().any(char::is_control)
                || entry.image_hosts.len() > 4
                || entry.image_hosts.iter().any(|host| {
                    !crate::platform::agenticos_external::valid_image_host(host)
                        || host.to_ascii_lowercase() != *host
                })
                || entry
                    .image_hosts
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != entry.image_hosts.len()
            {
                return Err(refused());
            }
        }
        Ok(metadata)
    }

    pub fn pin(&self, provider: &str, origin: &str) -> Option<&str> {
        self.providers
            .iter()
            .find(|entry| entry.provider == provider && entry.origin == origin)
            .map(|entry| entry.manifest_pin.as_str())
    }

    /// Image custody policy belongs to the same exact-origin deployment pin.
    pub fn image_hosts(&self, provider: &str, origin: &str) -> Option<&[String]> {
        self.providers
            .iter()
            .find(|entry| entry.provider == provider && entry.origin == origin)
            .map(|entry| entry.image_hosts.as_slice())
    }
}

fn verify(file: &std::fs::File, owner: u32, directory: bool) -> Result<()> {
    let md = file.metadata().map_err(|_| refused())?;
    if md.uid() != owner
        || md.mode() & 0o022 != 0
        || (directory && !md.is_dir())
        || (!directory && !md.is_file())
    {
        return Err(refused());
    }
    Ok(())
}

fn open_child(
    parent: &std::fs::File,
    name: &str,
    directory: bool,
) -> std::io::Result<std::fs::File> {
    let name =
        std::ffi::CString::new(name).map_err(|_| std::io::Error::other("invalid component"))?;
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

fn read_components(
    mut parent: std::fs::File,
    components: &[&str],
    owner: u32,
) -> Result<Option<DeploymentMetadata>> {
    verify(&parent, owner, true)?;
    for (index, name) in components.iter().enumerate() {
        let directory = index + 1 < components.len();
        let child = match open_child(&parent, name, directory) {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(refused()),
        };
        verify(&child, owner, directory)?;
        parent = child;
    }
    let mut bytes = Vec::new();
    parent
        .take(CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refused())?;
    DeploymentMetadata::parse(&bytes).map(Some)
}

/// Fixed production source: no path, UID or pin environment override.
pub fn load() -> Result<Option<DeploymentMetadata>> {
    let root = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(Path::new("/"))
        .map_err(|_| refused())?;
    read_components(root, &["etc", "cadence", "provider-deployments.json"], 0)
}

#[cfg(test)]
fn read_fixture(root: &Path, name: &str, owner: u32) -> Result<Option<DeploymentMetadata>> {
    let root = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(|_| refused())?;
    read_components(root, &[name], owner)
}

#[cfg(test)]
mod tests;
