//! Helper receives public image trust from independently signed RO media, NOT
//! from an Authorized response, peer UID, argv/environment or inherited FD.
use super::super::{refused, Result};
use super::{authenticate_image_attestation, decode, pin, Manifest};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
/// Public verification material only, never a signer or caller capability.
pub type PiPublicKey = (String, String, u64, [u8; 32]);
/// Authentication alone creates image trust, never owned caller/helper custody.
pub struct HelperImageTrust {
    manifest: Manifest,
}
impl HelperImageTrust {
    pub fn image(&self) -> &str {
        &self.manifest.image
    }
    pub fn helper_sha256(&self) -> Result<[u8; 32]> {
        pin(&self.manifest.artifacts.helper)
    }
    pub fn node_sha256(&self) -> Result<[u8; 32]> {
        pin(&self.manifest.artifacts.node)
    }
    pub fn profile_sha256(&self) -> Result<[u8; 32]> {
        pin(&self.manifest.artifacts.pi_graph)
    }
    pub fn policy_sha256(&self) -> Result<[u8; 32]> {
        pin(&self.manifest.artifacts.policy)
    }
    pub fn expires_at_ms(&self) -> u64 {
        self.manifest.expires_at_ms
    }
    pub fn pi_public_keys(&self) -> Result<Vec<PiPublicKey>> {
        self.manifest
            .pi_trust
            .as_ref()
            .ok_or_else(refused)?
            .iter()
            .map(|k| {
                Ok((
                    k.issuer.clone(),
                    k.kid.clone(),
                    k.key_version,
                    decode(&k.public_key, 32)?
                        .try_into()
                        .map_err(|_| refused())?,
                ))
            })
            .collect()
    }
}
/// Fixed canonical compact qualification, no caller path/key election. Empty
/// immutable image authority still refuses via the SAME bootstrap verifier.
pub fn helper_image_trust(now_ms: u64) -> Result<HelperImageTrust> {
    for path in ["/", "/opt", "/opt/protected"] {
        let m = std::fs::symlink_metadata(path).map_err(|_| refused())?;
        if !m.is_dir() || m.uid() != 0 || m.gid() != 0 || m.mode() & 0o7777 != 0o755 {
            return Err(refused());
        }
    }
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/opt/protected/image-qualification.jws")
        .map_err(|_| refused())?;
    let before = f.metadata().map_err(|_| refused())?;
    let mut fs: libc::statvfs = unsafe { std::mem::zeroed() };
    if !before.is_file()
        || before.uid() != 0
        || before.gid() != 0
        || before.mode() & 0o7777 != 0o644
        || before.nlink() != 1
        || before.size() == 0
        || before.size() > 32768
        || unsafe { libc::fstatvfs(f.as_raw_fd(), &mut fs) } != 0
        || fs.f_flag & libc::ST_RDONLY == 0
    {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    (&f).take(32769)
        .read_to_end(&mut bytes)
        .map_err(|_| refused())?;
    let after = f.metadata().map_err(|_| refused())?;
    if bytes.len() as u64 != before.size()
        || stamp(&before) != stamp(&after)
        || bytes.iter().any(|b| !b.is_ascii() || b"\r\n\0".contains(b))
    {
        return Err(refused());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| refused())?;
    let manifest = authenticate_image_attestation(text, now_ms, None, now_ms)?;
    Ok(HelperImageTrust { manifest })
}
fn stamp(m: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.size(),
        m.ctime(),
        m.ctime_nsec(),
        m.mtime(),
        m.mtime_nsec(),
    )
}
