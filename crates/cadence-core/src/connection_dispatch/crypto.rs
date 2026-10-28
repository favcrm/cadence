//! Optional custody crypto: the existing RustCrypto XChaCha20-Poly1305
//! framing with zeroized keys. Compiled only with `custody-crypto`.
//!
//! Native AAD is byte-identical to `src/platform/encrypted_sqlite.rs`
//! (`record_aad`/`header_aad`): same domain strings, the same u32 big-endian
//! schema/envelope versions, the same length-prefixed fields and the same
//! `credential_revision` u64 big-endian encoding. The hosted AAD uses separate
//! `hosted-v1` domains and a canonical tenant identity string, so the two can
//! never be reinterpreted as each other and a hosted workspace id is never
//! coerced into the native 32-hex workspace form.
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroizing;

pub const KEY_BYTES: usize = 32;
pub const KEY_ID_BYTES: usize = 16;
pub const NONCE_BYTES: usize = 24;
pub const TAG_BYTES: usize = 16;
pub const LOCATOR_BYTES: usize = 16;
pub const MAX_SECRET_BYTES: usize = 65_536;
pub const MAX_CIPHERTEXT_BYTES: usize = TAG_BYTES + MAX_SECRET_BYTES;
pub const SCHEMA_VERSION: u32 = 1;
pub const ENVELOPE_VERSION: u32 = 1;
pub const ALGORITHM: &str = "xchacha20poly1305";
pub const NATIVE_RECORD_DOMAIN: &str = "cadence.connections.custody";
pub const NATIVE_HEADER_DOMAIN: &str = "cadence.connections.custody.header";
pub const HOSTED_RECORD_DOMAIN: &str = "cadence.connections.custody.hosted-v1";
pub const HOSTED_HEADER_DOMAIN: &str = "cadence.connections.custody.hosted-header-v1";
/// Native open tolerates at most one block of padding beyond the contract.
const MAX_FIELD: usize = 64;
const MAX_EXCHANGE: usize = 16;

/// Private JSON uses the same canonical positive i64-bounded revision strings
/// as the public contract; the native AAD still encodes the u64 bytes.
mod revision_wire {
    use super::super::primitives::Revision;
    use serde::{Deserialize, Serialize};

    pub fn serialize<S: serde::Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        Revision::new(*value)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        Revision::deserialize(deserializer).map(|revision| revision.get())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoError {
    KeyLength,
    Integrity,
    Bounds,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CryptoError::KeyLength => "custody key is not 32 bytes",
            CryptoError::Integrity => "custody envelope failed authentication",
            CryptoError::Bounds => "custody envelope is outside its bounds",
        })
    }
}
impl std::error::Error for CryptoError {}

/// A 32-byte custody key. Zeroized on drop; never Debug/Serialize.
pub struct CustodyKey(Zeroizing<[u8; KEY_BYTES]>);

impl CustodyKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        let bytes: [u8; KEY_BYTES] = bytes.try_into().map_err(|_| CryptoError::KeyLength)?;
        Ok(Self(Zeroizing::new(bytes)))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new_from_slice(self.0.as_slice())
            .expect("a 32-byte key always makes an XChaCha20 cipher")
    }
}

fn push(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn prefix(out: &mut Vec<u8>, domain: &str) {
    push(out, domain.as_bytes());
    out.extend_from_slice(&SCHEMA_VERSION.to_be_bytes());
    out.extend_from_slice(&ENVELOPE_VERSION.to_be_bytes());
    push(out, ALGORITHM.as_bytes());
}

fn bordered(field: &str, max: usize) -> Result<(), CryptoError> {
    if field.is_empty() || field.len() > max {
        return Err(CryptoError::Bounds);
    }
    Ok(())
}

/// The native identity fields the record AAD authenticates. Not a secret:
/// only the identity is encoded (never the key or plaintext).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCustodyIdentityV1 {
    pub workspace_id: String,
    pub connection_id: String,
    pub provider: String,
    pub account: String,
    #[serde(with = "revision_wire")]
    pub credential_revision: u64,
    pub key_id: [u8; KEY_ID_BYTES],
    pub exchange: String,
}

impl NativeCustodyIdentityV1 {
    pub fn new(
        workspace_id: &str,
        connection_id: &str,
        provider: &str,
        account: &str,
        credential_revision: u64,
        key_id: [u8; KEY_ID_BYTES],
        exchange: &str,
    ) -> Result<Self, CryptoError> {
        for field in [workspace_id, connection_id, provider, account] {
            bordered(field, MAX_FIELD)?;
        }
        bordered(exchange, MAX_EXCHANGE)?;
        if credential_revision == 0 {
            return Err(CryptoError::Bounds);
        }
        Ok(Self {
            workspace_id: workspace_id.to_string(),
            connection_id: connection_id.to_string(),
            provider: provider.to_string(),
            account: account.to_string(),
            credential_revision,
            key_id,
            exchange: exchange.to_string(),
        })
    }
}

/// Borrowed native identity for callers that already own authenticated metadata.
/// Building AAD never needs a second owned copy of its strings.
pub struct NativeCustodyIdentityRefV1<'a> {
    pub workspace_id: &'a str,
    pub connection_id: &'a str,
    pub provider: &'a str,
    pub account: &'a str,
    pub credential_revision: u64,
    pub key_id: &'a [u8; KEY_ID_BYTES],
    pub exchange: &'a str,
}

/// Native record AAD v1 — byte-identical to `record_aad` in
/// `src/platform/encrypted_sqlite.rs`.
pub fn native_record_aad(
    identity: &NativeCustodyIdentityV1,
    locator: &[u8; LOCATOR_BYTES],
) -> Vec<u8> {
    native_record_aad_borrowed(
        NativeCustodyIdentityRefV1 {
            workspace_id: &identity.workspace_id,
            connection_id: &identity.connection_id,
            provider: &identity.provider,
            account: &identity.account,
            credential_revision: identity.credential_revision,
            key_id: &identity.key_id,
            exchange: &identity.exchange,
        },
        locator,
    )
}

/// The native framing shared with the zero-copy daemon metadata view.
pub fn native_record_aad_borrowed(
    identity: NativeCustodyIdentityRefV1<'_>,
    locator: &[u8; LOCATOR_BYTES],
) -> Vec<u8> {
    let prefix_bytes = 4 + NATIVE_RECORD_DOMAIN.len() + 8 + 4 + ALGORITHM.len();
    let field_bytes = [
        identity.workspace_id.len(),
        identity.connection_id.len(),
        identity.provider.len(),
        identity.account.len(),
        LOCATOR_BYTES,
        KEY_ID_BYTES,
        identity.exchange.len(),
    ]
    .into_iter()
    .map(|length| 4 + length)
    .sum::<usize>();
    let mut aad = Vec::with_capacity(prefix_bytes + field_bytes + 8);
    prefix(&mut aad, NATIVE_RECORD_DOMAIN);
    push(&mut aad, identity.workspace_id.as_bytes());
    push(&mut aad, identity.connection_id.as_bytes());
    push(&mut aad, identity.provider.as_bytes());
    push(&mut aad, identity.account.as_bytes());
    aad.extend_from_slice(&identity.credential_revision.to_be_bytes());
    push(&mut aad, locator);
    push(&mut aad, identity.key_id);
    push(&mut aad, identity.exchange.as_bytes());
    aad
}

/// Native header AAD v1 — byte-identical to `header_aad`.
pub fn native_header_aad(workspace_id: &str, key_id: &[u8; KEY_ID_BYTES]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(128);
    prefix(&mut aad, NATIVE_HEADER_DOMAIN);
    push(&mut aad, workspace_id.as_bytes());
    push(&mut aad, key_id);
    aad
}

/// The hosted identity: canonical tenant text plus the activation epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedCustodyIdentityV1 {
    pub tenant_workspace_id: String,
    pub connection_id: String,
    pub provider: String,
    pub account: String,
    #[serde(with = "revision_wire")]
    pub credential_revision: u64,
    pub key_id: [u8; KEY_ID_BYTES],
    #[serde(with = "revision_wire")]
    pub custody_epoch: u64,
}

impl HostedCustodyIdentityV1 {
    pub fn new(
        tenant_workspace_id: &str,
        connection_id: &str,
        provider: &str,
        account: &str,
        credential_revision: u64,
        key_id: [u8; KEY_ID_BYTES],
        custody_epoch: u64,
    ) -> Result<Self, CryptoError> {
        for field in [tenant_workspace_id, connection_id, provider, account] {
            bordered(field, MAX_FIELD)?;
        }
        if credential_revision == 0 || custody_epoch == 0 {
            return Err(CryptoError::Bounds);
        }
        Ok(Self {
            tenant_workspace_id: tenant_workspace_id.to_string(),
            connection_id: connection_id.to_string(),
            provider: provider.to_string(),
            account: account.to_string(),
            credential_revision,
            key_id,
            custody_epoch,
        })
    }
}

/// Hosted record AAD: a separate domain and canonical tenant identity, with
/// the activation epoch bound in.
pub fn hosted_record_aad(
    identity: &HostedCustodyIdentityV1,
    locator: &[u8; LOCATOR_BYTES],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(288);
    prefix(&mut aad, HOSTED_RECORD_DOMAIN);
    push(&mut aad, identity.tenant_workspace_id.as_bytes());
    push(&mut aad, identity.connection_id.as_bytes());
    push(&mut aad, identity.provider.as_bytes());
    push(&mut aad, identity.account.as_bytes());
    aad.extend_from_slice(&identity.credential_revision.to_be_bytes());
    push(&mut aad, locator);
    push(&mut aad, &identity.key_id);
    aad.extend_from_slice(&identity.custody_epoch.to_be_bytes());
    aad
}

pub fn hosted_header_aad(
    tenant_workspace_id: &str,
    key_id: &[u8; KEY_ID_BYTES],
    custody_epoch: u64,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(160);
    prefix(&mut aad, HOSTED_HEADER_DOMAIN);
    push(&mut aad, tenant_workspace_id.as_bytes());
    push(&mut aad, key_id);
    aad.extend_from_slice(&custody_epoch.to_be_bytes());
    aad
}

/// Seal plaintext with the exact XChaCha20-Poly1305 framing the vault uses:
/// `ciphertext || 16-byte tag`, authenticated against the supplied AAD.
pub fn seal(
    key: &CustodyKey,
    nonce: &[u8; NONCE_BYTES],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if plaintext.len() > MAX_SECRET_BYTES {
        return Err(CryptoError::Bounds);
    }
    let ciphertext = key
        .cipher()
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Integrity)?;
    if ciphertext.len() > MAX_CIPHERTEXT_BYTES {
        return Err(CryptoError::Bounds);
    }
    Ok(ciphertext)
}

/// Open a sealed value. Corruption, a wrong identity/AAD or a wrong key
/// refuses without a plaintext fallback.
pub fn open(
    key: &CustodyKey,
    nonce: &[u8; NONCE_BYTES],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if ciphertext.len() < TAG_BYTES || ciphertext.len() > MAX_CIPHERTEXT_BYTES {
        return Err(CryptoError::Bounds);
    }
    let plaintext = key
        .cipher()
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Integrity)?;
    Ok(Zeroizing::new(plaintext))
}

pub fn seal_native_record(
    key: &CustodyKey,
    identity: &NativeCustodyIdentityV1,
    locator: &[u8; LOCATOR_BYTES],
    nonce: &[u8; NONCE_BYTES],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    seal(key, nonce, &native_record_aad(identity, locator), plaintext)
}

pub fn open_native_record(
    key: &CustodyKey,
    identity: &NativeCustodyIdentityV1,
    locator: &[u8; LOCATOR_BYTES],
    nonce: &[u8; NONCE_BYTES],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    open(
        key,
        nonce,
        &native_record_aad(identity, locator),
        ciphertext,
    )
}

/// The native header seals an empty plaintext; the stored bytes are the tag.
pub fn seal_native_header(
    key: &CustodyKey,
    workspace_id: &str,
    key_id: &[u8; KEY_ID_BYTES],
    nonce: &[u8; NONCE_BYTES],
) -> Result<Vec<u8>, CryptoError> {
    bordered(workspace_id, MAX_FIELD)?;
    seal(key, nonce, &native_header_aad(workspace_id, key_id), b"")
}

pub fn open_native_header(
    key: &CustodyKey,
    workspace_id: &str,
    key_id: &[u8; KEY_ID_BYTES],
    nonce: &[u8; NONCE_BYTES],
    tag: &[u8],
) -> Result<(), CryptoError> {
    if tag.len() != TAG_BYTES {
        return Err(CryptoError::Bounds);
    }
    open(key, nonce, &native_header_aad(workspace_id, key_id), tag).map(|_| ())
}

pub fn seal_hosted_record(
    key: &CustodyKey,
    identity: &HostedCustodyIdentityV1,
    locator: &[u8; LOCATOR_BYTES],
    nonce: &[u8; NONCE_BYTES],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    seal(key, nonce, &hosted_record_aad(identity, locator), plaintext)
}

pub fn open_hosted_record(
    key: &CustodyKey,
    identity: &HostedCustodyIdentityV1,
    locator: &[u8; LOCATOR_BYTES],
    nonce: &[u8; NONCE_BYTES],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    open(
        key,
        nonce,
        &hosted_record_aad(identity, locator),
        ciphertext,
    )
}

pub fn seal_hosted_header(
    key: &CustodyKey,
    tenant_workspace_id: &str,
    key_id: &[u8; KEY_ID_BYTES],
    custody_epoch: u64,
    nonce: &[u8; NONCE_BYTES],
) -> Result<Vec<u8>, CryptoError> {
    bordered(tenant_workspace_id, MAX_FIELD)?;
    if custody_epoch == 0 {
        return Err(CryptoError::Bounds);
    }
    seal(
        key,
        nonce,
        &hosted_header_aad(tenant_workspace_id, key_id, custody_epoch),
        b"",
    )
}

pub fn open_hosted_header(
    key: &CustodyKey,
    tenant_workspace_id: &str,
    key_id: &[u8; KEY_ID_BYTES],
    custody_epoch: u64,
    nonce: &[u8; NONCE_BYTES],
    tag: &[u8],
) -> Result<(), CryptoError> {
    if tag.len() != TAG_BYTES {
        return Err(CryptoError::Bounds);
    }
    open(
        key,
        nonce,
        &hosted_header_aad(tenant_workspace_id, key_id, custody_epoch),
        tag,
    )
    .map(|_| ())
}
