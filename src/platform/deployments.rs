//! Image-owned assertions about provider deployments, never remote discovery.
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use crate::error::{Error, Result};
use serde::Deserialize;

const CAP: u64 = 16 * 1024;

#[derive(Clone, Debug)]
pub struct DeploymentMetadata {
    providers: Vec<ProviderDeployment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataFile {
    schema: u32,
    providers: Vec<ProviderDeployment>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderDeployment {
    provider: String,
    origin: String,
    manifest_pin: String,
    /// CAD-816: accepted for older metadata files; media artifacts now come
    /// from the AgenticOS door itself, so the CDN allowlist is ignored.
    #[serde(default)]
    #[allow(dead_code)]
    image_hosts: Vec<String>,
    // A present null is not absence: deserialize the assertion itself first.
    #[serde(default, deserialize_with = "parse_transport")]
    transport: Option<DeploymentTransport>,
}

#[derive(Clone, Debug)]
enum DeploymentTransport {
    HostedMediaLease,
    /// CAD-1158: dedicated bridge-owned SMTP admission. Binds the fixed
    /// `smtp.internal` relay contract only; never media or lease authority.
    HostedSmtpRelay,
}

/// CAD-1158: the exact image-owned SMTP admission tuple. Only this tuple
/// admits the credential-carrying relay; no env URL, app input or other
/// provider/origin/pin combination does.
pub(crate) const SMTP_PROVIDER: &str = "smtp";
pub(crate) const SMTP_ORIGIN: &str = "http://smtp.internal";
pub(crate) const SMTP_MANIFEST_PIN: &str = "smtp-internal-relay@2";

fn parse_transport<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<DeploymentTransport>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match String::deserialize(deserializer)?.as_str() {
        "hosted-media-lease@1" => Ok(Some(DeploymentTransport::HostedMediaLease)),
        "hosted-smtp-relay@1" => Ok(Some(DeploymentTransport::HostedSmtpRelay)),
        _ => Err(serde::de::Error::custom(
            "unknown provider transport assertion",
        )),
    }
}

/// The reviewed hosted media manifest pins: @3 (media + source, text stays
/// refused) and @4 (adds `generate_text`). Anything else is not a pin.
fn reviewed_media_pin(pin: &str) -> Option<&'static str> {
    [
        crate::platform::agenticos_external::MANIFEST_PIN,
        crate::platform::agenticos_external::TEXT_MANIFEST_PIN,
    ]
    .into_iter()
    .find(|reviewed| *reviewed == pin)
}

/// Only validated image metadata can produce this construction proof. It
/// authorizes one fixed internal media door, not arbitrary HTTP or a lease.
#[derive(Clone, Debug)]
pub(crate) struct HostedMediaAdmission {
    /// The reviewed manifest pin the image metadata asserted — only
    /// `MANIFEST_PIN` (@3) or `TEXT_MANIFEST_PIN` (@4), never a free string.
    pin: &'static str,
}

impl HostedMediaAdmission {
    pub(crate) fn pin(&self) -> &'static str {
        self.pin
    }
}

/// CAD-1158: only validated image metadata carrying the exact SMTP tuple
/// can produce this construction proof. It authorizes one fixed internal
/// relay (`http://smtp.internal`), never an arbitrary URL, media
/// authority, hosted email or a lifecycle lease.
#[derive(Clone, Debug)]
pub(crate) struct HostedSmtpAdmission {
    _private: (),
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
        let metadata: MetadataFile = serde_json::from_slice(bytes).map_err(|_| refused())?;
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
                // A transport assertion admits exactly its own reviewed
                // tuple: the MEDIA lease or the CAD-1158 SMTP relay. Any
                // other provider/origin/pin combination carrying a
                // transport is refused fail-closed.
                || entry.transport.as_ref().is_some_and(|transport| {
                    match transport {
                        DeploymentTransport::HostedMediaLease => {
                            entry.provider != crate::platform::agenticos_external::PLATFORM
                                || entry.origin != "http://api.internal"
                                || reviewed_media_pin(&entry.manifest_pin).is_none()
                        }
                        DeploymentTransport::HostedSmtpRelay => {
                            entry.provider != SMTP_PROVIDER
                                || entry.origin != SMTP_ORIGIN
                                || entry.manifest_pin != SMTP_MANIFEST_PIN
                        }
                    }
                })
                || entry.manifest_pin.is_empty()
                || entry.manifest_pin.len() > 256
                || entry.manifest_pin.chars().any(char::is_control)
            {
                return Err(refused());
            }
        }
        Ok(Self {
            providers: metadata.providers,
        })
    }

    /// The MEDIA assertion grants media only. CAD-1158: an explicit
    /// transport match, so an SMTP-only assertion never yields a media
    /// admission (the find-first-transport pitfall).
    pub(crate) fn hosted_media(&self) -> Option<HostedMediaAdmission> {
        self.providers
            .iter()
            .find(|entry| matches!(entry.transport, Some(DeploymentTransport::HostedMediaLease)))
            .and_then(|entry| reviewed_media_pin(&entry.manifest_pin))
            .map(|pin| HostedMediaAdmission { pin })
    }

    /// CAD-1158: the dedicated SMTP admission. `Some` only for the exact
    /// reviewed tuple (provider `smtp`, fixed `http://smtp.internal`
    /// origin, `smtp-internal-relay@2` pin, `hosted-smtp-relay@1`
    /// transport) — all four fields, fail-closed on any mismatch.
    /// A MEDIA assertion never yields this proof.
    pub(crate) fn hosted_smtp(&self) -> Option<HostedSmtpAdmission> {
        self.providers
            .iter()
            .find(|entry| {
                entry.provider == SMTP_PROVIDER
                    && entry.origin == SMTP_ORIGIN
                    && entry.manifest_pin == SMTP_MANIFEST_PIN
                    && matches!(entry.transport, Some(DeploymentTransport::HostedSmtpRelay))
            })
            .map(|_| HostedSmtpAdmission { _private: () })
    }

    pub fn pin(&self, provider: &str, origin: &str) -> Option<&str> {
        self.providers
            .iter()
            .find(|entry| entry.provider == provider && entry.origin == origin)
            .map(|entry| entry.manifest_pin.as_str())
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
