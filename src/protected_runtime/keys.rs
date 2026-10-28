//! The root owner's Ed25519 authority key and the private broker-fence
//! signature material.
//!
//! The key lives outside the authority SQLite, in a root-owned `0600` file the
//! profile names; its public half is pinned in the profile. The daemon never
//! reads it. It signs one typed document — the broker-fence request — proving
//! possession of the root owner key; combined with the kernel `SO_PEERCRED` uid
//! 0 fact on the private socket, this authenticates the root owner without the
//! daemon needing `CAP_SYS_PTRACE` to read a root `/proc/<pid>/exe`. Host root
//! remains outside the guarantee by the design's own residual-trust statement.
//!
//! No generic sign/eval/key-getter API exists: the only signing entry point
//! takes the exact fence fields.

use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};

use crate::error::{Error, Result};

use super::profile::LocalProfile;
use super::registry;

const FENCE_DOMAIN: &[u8] = b"cadence.broker-fence.v1\0";

pub(crate) struct OwnerSigner {
    pair: Ed25519KeyPair,
}

impl OwnerSigner {
    /// Test-only: wrap an in-memory key. Never a production key source.
    #[cfg(test)]
    pub(crate) fn test_signer(pair: Ed25519KeyPair) -> Self {
        Self { pair }
    }

    /// Load the owner key and require its public half to equal the profile pin.
    /// Root-owned `0600` regular file, bounded, no symlink.
    pub(crate) fn load(profile: &LocalProfile) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let path = profile.owner_private_key_file();
        let meta = std::fs::symlink_metadata(path)
            .map_err(|_| Error::rejected("owner signing key is missing"))?;
        if !meta.is_file() || meta.is_symlink() || meta.uid() != 0 || meta.mode() & 0o077 != 0 {
            return Err(Error::rejected(
                "owner signing key is not the root-owned private file",
            ));
        }
        if meta.len() == 0 || meta.len() > 4096 {
            return Err(Error::rejected(
                "owner signing key size is outside the bound",
            ));
        }
        let bytes = zeroize::Zeroizing::new(
            std::fs::read(path).map_err(|_| Error::rejected("owner signing key is unreadable"))?,
        );
        // OpenSSL emits PKCS#8 v1; ring's strict parser accepts only v2.
        // Both formats derive the public key, which MUST match the pin below.
        let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&bytes)
            .map_err(|_| Error::rejected("owner signing key is not Ed25519 PKCS#8"))?;
        let public: [u8; 32] = pair
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| Error::rejected("owner signing key has no 32-byte public half"))?;
        if &public != profile.owner_public_key() {
            return Err(Error::rejected(
                "owner signing key does not match the pinned owner public key",
            ));
        }
        Ok(Self { pair })
    }

    fn sign(&self, material: &[u8]) -> Vec<u8> {
        self.pair.sign(material).as_ref().to_vec()
    }

    /// Domain-separated signing used by the R5 peer/installer proofs.
    pub(crate) fn sign_domain(&self, material: &[u8]) -> Vec<u8> {
        self.sign(material)
    }
}

/// Verify a proof signature with a pinned 32-byte Ed25519 public key.
pub(crate) fn verify_with_public_key(
    public_key: &[u8; 32],
    material: &[u8],
    signature: &[u8],
) -> Result<()> {
    if signature.len() != 64 {
        return Err(Error::rejected("proof signature is not 64 bytes"));
    }
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(material, signature)
        .map_err(|_| Error::rejected("proof signature does not verify"))
}

/// The exact broker-fence signature material: domain tag plus canonical JSON of
/// the request fields (sorted keys, no whitespace). The `signature` field is not
/// part of the document.
pub(crate) fn fence_material(
    runtime_id: &str,
    owner_epoch: u64,
    generation: u64,
    reason: &str,
    nonce: &str,
) -> Result<Vec<u8>> {
    let document = serde_json::json!({
        "v": 1,
        "kind": "broker_fence",
        "runtime_id": runtime_id,
        "owner_epoch": owner_epoch,
        "expected_generation": generation,
        "reason": reason,
        "nonce": nonce,
    });
    let canonical = registry::canonical(&document)?;
    let mut material = Vec::with_capacity(FENCE_DOMAIN.len() + canonical.len());
    material.extend_from_slice(FENCE_DOMAIN);
    material.extend_from_slice(&canonical);
    Ok(material)
}

/// Sign one fence request. The only signing entry point.
pub(crate) fn sign_fence(
    signer: &OwnerSigner,
    runtime_id: &str,
    owner_epoch: u64,
    generation: u64,
    reason: &str,
    nonce: &str,
) -> Result<Vec<u8>> {
    let material = fence_material(runtime_id, owner_epoch, generation, reason, nonce)?;
    Ok(signer.sign(&material))
}

/// Verify a fence signature with the profile-pinned owner public key.
pub(crate) fn verify_fence_signature(
    profile: &LocalProfile,
    material: &[u8],
    signature: &[u8],
) -> Result<()> {
    if signature.len() != 64 {
        return Err(Error::rejected("broker fence signature is not 64 bytes"));
    }
    UnparsedPublicKey::new(&ED25519, profile.owner_public_key())
        .verify(material, signature)
        .map_err(|_| Error::rejected("broker fence signature does not verify"))
}

pub(crate) fn b64(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn decode_b64(s: &str) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    if s.len() > 128 {
        return Err(Error::rejected("signature is too long"));
    }
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::rejected("signature is not base64url"))
}
