//! CAD-2215: opaque, bound cursors and stable keyed row identifiers.
//!
//! A scoped reader gets a fixed-length opaque cursor instead of a global
//! sequence value. The cursor is an XChaCha20-Poly1305 envelope whose
//! encrypted payload carries only a nonnegative position; the fixed-size
//! digest of every binding field the server admitted for that page is
//! authenticated as AAD:
//!
//! * domain/purpose,
//! * page direction,
//! * principal,
//! * the exact immutable resource/incarnation,
//! * canonical filters,
//! * the codec's workspace namespace.
//!
//! The digest is length-framed, so no two different field tuples can produce
//! the same authenticated binding. Opening a cursor under another binding,
//! domain, direction, workspace or key is an authentication failure with one
//! generic error. Tokens are not authorization: the caller must re-prove the
//! reader's scope on every use before it even opens a cursor.
//!
//! Keys are supplied already admitted by the operator. This module never
//! reads, writes, provisions or regenerates key material, assigns no expiry
//! and retains every key id in the ring so a token sealed before a rotation
//! still opens. [`ScopedCursorCodec::keyring_fingerprint`] and
//! [`ScopedCursorCodec::verify_keyring_fingerprint`] let the manager bind an
//! explicitly provisioned ring to persisted workspace metadata without
//! creating anything when metadata is absent or mismatched.
//!
//! `private_available()` is unaffected: this is the cursor codec only, not
//! OS isolation.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Tag, XChaCha20Poly1305, XNonce};
use std::fmt;
use zeroize::Zeroizing;

/// AEAD key length in bytes.
pub(crate) const KEY_BYTES: usize = 32;
/// Key id length in bytes. Ids are public selectors, not secrets.
pub(crate) const KEY_ID_BYTES: usize = 16;
/// Canonical unpadded base64 length of a full 32-byte MAC.
const MAC_TEXT_CHARS: usize = KEY_BYTES / 3 * 4
    + match KEY_BYTES % 3 {
        0 => 0,
        1 => 2,
        _ => 3,
    };

const NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
const POSITION_BYTES: usize = 8;
const BINDING_BYTES: usize = 32;

/// Clear envelope header: magic, version, key id, nonce, ciphertext and tag.
const MAGIC: [u8; 4] = *b"SCUR";
const ENVELOPE_VERSION: u8 = 1;
const OFF_VERSION: usize = MAGIC.len();
const OFF_KEY_ID: usize = OFF_VERSION + 1;
const OFF_NONCE: usize = OFF_KEY_ID + KEY_ID_BYTES;
const OFF_CIPHER: usize = OFF_NONCE + NONCE_BYTES;
const OFF_TAG: usize = OFF_CIPHER + POSITION_BYTES;
const ENVELOPE_BYTES: usize = OFF_TAG + TAG_BYTES;

/// A textual discriminator keeps cursors out of numeric cursor slots.
const TOKEN_PREFIX: &str = "csc1_";
const TOKEN_BODY_CHARS: usize = ENVELOPE_BYTES / 3 * 4
    + match ENVELOPE_BYTES % 3 {
        0 => 0,
        1 => 2,
        _ => 3,
    };
const TOKEN_CHARS: usize = TOKEN_PREFIX.len() + TOKEN_BODY_CHARS;

const BINDING_DOMAIN: &[u8] = b"cadence.scoped-cursor.binding.v1";
const BINDING_VERSION: u8 = 1;
const ENVELOPE_AAD_DOMAIN: &[u8] = b"cadence.scoped-cursor.envelope.v1";
const ROW_ID_DOMAIN: &[u8] = b"cadence.scoped-cursor.row-id.v1";
const ROW_ID_VERSION: u8 = 1;
const FINGERPRINT_DOMAIN: &[u8] = b"cadence.scoped-cursor.keyring-fingerprint.v1";
const FINGERPRINT_VERSION: u8 = 1;

const ROW_ID_PREFIX: &str = "csrid_";
const FINGERPRINT_PREFIX: &str = "csfk_";

const DOMAIN_MAX: usize = 128;
/// The signed bearer is capped at 8192 bytes before base64url decoding,
/// leaving room here for its decoded principal fields and tuple framing.
/// Operator-proven `read_as` identities share this fail-closed bound.
const PRINCIPAL_MAX: usize = 8192;
const RESOURCE_MAX: usize = 512;
const FILTERS_MAX: usize = 4096;
const WORKSPACE_MAX: usize = 256;
const ROW_MAX: usize = 512;

const AAD_BYTES: usize = ENVELOPE_AAD_DOMAIN.len() + 1 + KEY_ID_BYTES + NONCE_BYTES + BINDING_BYTES;

/// One generic rejection for invalid material and every authentication or
/// bounds failure. It deliberately carries no token, key, field or position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorError;

impl fmt::Display for CursorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("scoped cursor rejected")
    }
}

impl std::error::Error for CursorError {}

/// A public key selector. It is safe to log; it is not key material.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CursorKeyId([u8; KEY_ID_BYTES]);

impl CursorKeyId {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, CursorError> {
        let bytes: [u8; KEY_ID_BYTES] = bytes.try_into().map_err(|_| CursorError)?;
        Ok(Self(bytes))
    }

    pub(crate) fn as_bytes(&self) -> &[u8; KEY_ID_BYTES] {
        &self.0
    }
}

impl fmt::Debug for CursorKeyId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CursorKeyId(")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str(")")
    }
}

struct CursorKey {
    id: CursorKeyId,
    key: Zeroizing<[u8; KEY_BYTES]>,
}

impl CursorKey {
    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new_from_slice(&self.key[..])
            .expect("a 32-byte key always makes an XChaCha20 cipher")
    }
}

/// The already-admitted persistent keyring. It contains no active-key
/// policy and no expiry: the codec keeps every entry for decryption.
#[derive(Default)]
pub(crate) struct CursorKeyring {
    keys: Vec<CursorKey>,
}

impl CursorKeyring {
    pub(crate) fn new() -> Self {
        Self { keys: Vec::new() }
    }

    /// Add one admitted key. Duplicate ids are refused rather than
    /// resolved by ordering.
    pub(crate) fn insert_key(&mut self, id: CursorKeyId, key: &[u8]) -> Result<(), CursorError> {
        if self.keys.iter().any(|entry| entry.id == id) || key.len() != KEY_BYTES {
            return Err(CursorError);
        }
        let mut copy = Zeroizing::new([0u8; KEY_BYTES]);
        copy.as_mut_slice().copy_from_slice(key);
        self.keys.push(CursorKey { id, key: copy });
        Ok(())
    }
}

impl fmt::Debug for CursorKeyring {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CursorKeyring")
            .field("key_count", &self.keys.len())
            .finish_non_exhaustive()
    }
}

/// Page direction bound into the cursor. Forward and backward cursors are
/// never interchangeable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorDirection {
    Forward,
    Backward,
}

impl CursorDirection {
    fn tag(self) -> u8 {
        match self {
            Self::Forward => 1,
            Self::Backward => 2,
        }
    }
}

/// The server-supplied, already-canonical binding for one cursor. The
/// workspace namespace is injected from the codec, never from this struct,
/// so a token cannot be replayed across workspaces.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CursorBinding<'a> {
    /// Domain/purpose, e.g. `thread`, `message-list`, `jobs`. Never a
    /// human label; it is an exact contract term.
    pub domain: &'a str,
    pub direction: CursorDirection,
    /// Opaque stable principal identity. Not an auth token.
    pub principal: &'a str,
    /// Exact immutable resource or resource/incarnation.
    pub resource: &'a str,
    /// Canonical filter text (e.g. sorted, normalized query terms). The
    /// exact bytes are bound; callers must canonicalize before sealing.
    pub filters: &'a str,
}

/// A fixed-length, defensively copied cursor codec.
pub(crate) struct ScopedCursorCodec {
    workspace: String,
    keys: Vec<CursorKey>,
    active_key_id: CursorKeyId,
    identity_key: Zeroizing<[u8; KEY_BYTES]>,
    keyring_fingerprint: String,
}

impl ScopedCursorCodec {
    /// Build a codec from an explicitly admitted keyring. `active_key_id`
    /// must already exist in the ring; missing keys, an empty ring, a
    /// duplicate id, malformed key bytes or an identity key byte-identical to
    /// any AEAD key are refused, never generated.
    pub(crate) fn new(
        workspace: &str,
        mut keyring: CursorKeyring,
        active_key_id: CursorKeyId,
        identity_key: &[u8],
    ) -> Result<Self, CursorError> {
        if !canonical_text(workspace, WORKSPACE_MAX, false) {
            return Err(CursorError);
        }
        let identity_key = if identity_key.len() == KEY_BYTES {
            let mut copy = Zeroizing::new([0u8; KEY_BYTES]);
            copy.as_mut_slice().copy_from_slice(identity_key);
            copy
        } else {
            return Err(CursorError);
        };
        if keyring.keys.is_empty() {
            return Err(CursorError);
        }
        if !keyring.keys.iter().any(|entry| entry.id == active_key_id) {
            return Err(CursorError);
        }
        // The identity key must be separate material: accidental reuse would
        // blur the row-id and AEAD key domains.
        if keyring
            .keys
            .iter()
            .any(|entry| constant_time_slices_equal(&identity_key[..], &entry.key[..]))
        {
            return Err(CursorError);
        }
        keyring.keys.sort_by_key(|entry| entry.id.0);
        if keyring.keys.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(CursorError);
        }
        let keyring_fingerprint =
            compute_keyring_fingerprint(workspace, &keyring.keys, &identity_key[..]);
        Ok(Self {
            workspace: workspace.to_string(),
            keys: keyring.keys,
            active_key_id,
            identity_key,
            keyring_fingerprint,
        })
    }

    /// A keyed, domain-separated fingerprint of the workspace namespace, the
    /// identity key and every retained key id plus key bytes. It deliberately
    /// excludes the active key id, so switching the active key among the same
    /// retained set keeps the persisted check and old cursors valid. Adding,
    /// removing or substituting key material changes it and must fail closed
    /// until an explicitly approved transition. It reveals no key material.
    pub(crate) fn keyring_fingerprint(&self) -> &str {
        &self.keyring_fingerprint
    }

    /// Constant-time comparison against persisted metadata. A mismatch is
    /// the same generic refusal; the codec does not initialize or repair a
    /// ring.
    pub(crate) fn verify_keyring_fingerprint(&self, expected: &str) -> Result<(), CursorError> {
        if constant_time_slices_equal(expected.as_bytes(), self.keyring_fingerprint.as_bytes()) {
            Ok(())
        } else {
            Err(CursorError)
        }
    }

    /// Seal one nonnegative position under exactly this binding using the
    /// active key and a fresh OS-CSPRNG nonce.
    pub(crate) fn seal(
        &self,
        position: i64,
        binding: &CursorBinding<'_>,
    ) -> Result<String, CursorError> {
        if position < 0 {
            return Err(CursorError);
        }
        let binding_digest = self.binding_digest(binding)?;
        let key = self.key_for(self.active_key_id).ok_or(CursorError)?;
        let mut nonce = [0u8; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|_| CursorError)?;
        let mut position_bytes = [0u8; POSITION_BYTES];
        position_bytes.copy_from_slice(&(position as u64).to_be_bytes());
        let header = envelope_aad(self.active_key_id.as_bytes(), &nonce, &binding_digest);
        let tag = key
            .cipher()
            .encrypt_in_place_detached(XNonce::from_slice(&nonce), &header, &mut position_bytes)
            .map_err(|_| CursorError)?;
        let mut envelope = [0u8; ENVELOPE_BYTES];
        envelope[..MAGIC.len()].copy_from_slice(&MAGIC);
        envelope[OFF_VERSION] = ENVELOPE_VERSION;
        envelope[OFF_KEY_ID..OFF_NONCE].copy_from_slice(self.active_key_id.as_bytes());
        envelope[OFF_NONCE..OFF_CIPHER].copy_from_slice(&nonce);
        envelope[OFF_CIPHER..OFF_TAG].copy_from_slice(&position_bytes);
        envelope[OFF_TAG..].copy_from_slice(tag.as_slice());
        let mut token = String::with_capacity(TOKEN_CHARS);
        token.push_str(TOKEN_PREFIX);
        URL_SAFE_NO_PAD.encode_string(envelope, &mut token);
        debug_assert_eq!(token.len(), TOKEN_CHARS);
        Ok(token)
    }

    /// Open a cursor under exactly one binding. Any tamper, another key,
    /// domain, direction, filter, resource, principal or workspace, or a
    /// negative/out-of-range position is the same generic refusal.
    pub(crate) fn open(
        &self,
        token: &str,
        binding: &CursorBinding<'_>,
    ) -> Result<i64, CursorError> {
        let binding_digest = self.binding_digest(binding)?;
        let mut envelope = [0u8; ENVELOPE_BYTES];
        decode_envelope(token, &mut envelope)?;
        if envelope[..MAGIC.len()] != MAGIC[..] || envelope[OFF_VERSION] != ENVELOPE_VERSION {
            return Err(CursorError);
        }
        let key_id = CursorKeyId::from_bytes(&envelope[OFF_KEY_ID..OFF_NONCE])?;
        let key = self.key_for(key_id).ok_or(CursorError)?;
        let mut nonce = [0u8; NONCE_BYTES];
        nonce.copy_from_slice(&envelope[OFF_NONCE..OFF_CIPHER]);
        let header = envelope_aad(key_id.as_bytes(), &nonce, &binding_digest);
        let mut position_bytes = [0u8; POSITION_BYTES];
        position_bytes.copy_from_slice(&envelope[OFF_CIPHER..OFF_TAG]);
        let tag = Tag::from_slice(&envelope[OFF_TAG..]);
        key.cipher()
            .decrypt_in_place_detached(
                XNonce::from_slice(&nonce),
                &header,
                &mut position_bytes,
                tag,
            )
            .map_err(|_| CursorError)?;
        let position = u64::from_be_bytes(position_bytes);
        if position > i64::MAX as u64 {
            return Err(CursorError);
        }
        Ok(position as i64)
    }

    /// A stable, domain-separated row identifier. It is keyed by the separate
    /// identity key over the durable workspace namespace plus exactly the
    /// immutable domain, resource and row identity; principal, mutable payload
    /// and numeric positions are not inputs. `row` is bytes so an internal
    /// numeric key can pass `i64::to_be_bytes()` without a String allocation;
    /// the u32 length prefix keeps the framing unambiguous. A portable import
    /// creates a new namespace and therefore new identifiers even when the
    /// identity key is reused. The result is `csrid_` plus canonical
    /// URL_SAFE_NO_PAD base64 of the full 32-byte HMAC.
    pub(crate) fn stable_row_id(
        &self,
        domain: &str,
        resource: &str,
        row: &[u8],
    ) -> Result<String, CursorError> {
        if !canonical_text(domain, DOMAIN_MAX, false)
            || !canonical_text(resource, RESOURCE_MAX, false)
            || row.is_empty()
            || row.len() > ROW_MAX
        {
            return Err(CursorError);
        }
        let mut context = hmac_context(&self.identity_key[..]);
        update_framed_hmac(&mut context, ROW_ID_DOMAIN);
        context.update(&[ROW_ID_VERSION]);
        update_framed_hmac(&mut context, self.workspace.as_bytes());
        update_framed_hmac(&mut context, domain.as_bytes());
        update_framed_hmac(&mut context, resource.as_bytes());
        update_framed_hmac(&mut context, row);
        let tag = context.sign();
        let mut id = String::with_capacity(ROW_ID_PREFIX.len() + MAC_TEXT_CHARS);
        id.push_str(ROW_ID_PREFIX);
        URL_SAFE_NO_PAD.encode_string(tag, &mut id);
        Ok(id)
    }

    /// Validate one server-supplied binding against the same bounds [`seal`]
    /// and [`open`] enforce, without sealing or opening anything. Consumers
    /// use it so a page that happens to have no rows — and would otherwise
    /// never touch the codec — still fails closed on an out-of-bounds binding
    /// instead of answering an empty success.
    ///
    /// [`seal`]: Self::seal
    /// [`open`]: Self::open
    pub(crate) fn validate_binding(&self, binding: &CursorBinding<'_>) -> Result<(), CursorError> {
        self.binding_digest(binding).map(|_| ())
    }

    fn key_for(&self, id: CursorKeyId) -> Option<&CursorKey> {
        self.keys.iter().find(|entry| entry.id == id)
    }

    fn binding_digest(
        &self,
        binding: &CursorBinding<'_>,
    ) -> Result<[u8; BINDING_BYTES], CursorError> {
        if !canonical_text(binding.domain, DOMAIN_MAX, false)
            // The principal is opaque: its producer already length-frames it,
            // it is only ever hashed, and it is never parsed, logged or used
            // as a path. Verified board/CLI subjects are opaque strings (the
            // claim checks are nonempty + a byte bound, with no character
            // ban), so rejecting NUL/control bytes here would refuse an
            // authorized reader; only the size bound applies. Framing keeps
            // the exact bytes unambiguous.
            || !bounded_text(binding.principal, PRINCIPAL_MAX, false)
            || !canonical_text(binding.resource, RESOURCE_MAX, false)
            || !canonical_text(binding.filters, FILTERS_MAX, true)
        {
            return Err(CursorError);
        }
        let mut context = ring::digest::Context::new(&ring::digest::SHA256);
        update_framed_digest(&mut context, BINDING_DOMAIN);
        context.update(&[BINDING_VERSION]);
        context.update(&[binding.direction.tag()]);
        update_framed_digest(&mut context, binding.domain.as_bytes());
        update_framed_digest(&mut context, binding.principal.as_bytes());
        update_framed_digest(&mut context, binding.resource.as_bytes());
        update_framed_digest(&mut context, binding.filters.as_bytes());
        update_framed_digest(&mut context, self.workspace.as_bytes());
        let digest = context.finish();
        let mut out = [0u8; BINDING_BYTES];
        out.copy_from_slice(digest.as_ref());
        Ok(out)
    }
}

impl fmt::Debug for ScopedCursorCodec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedCursorCodec")
            .field("workspace", &self.workspace)
            .field("active_key_id", &self.active_key_id)
            .field("retained_key_count", &self.keys.len())
            .finish_non_exhaustive()
    }
}

fn compute_keyring_fingerprint(workspace: &str, keys: &[CursorKey], identity_key: &[u8]) -> String {
    let mut context = hmac_context(identity_key);
    update_framed_hmac(&mut context, FINGERPRINT_DOMAIN);
    context.update(&[FINGERPRINT_VERSION]);
    update_framed_hmac(&mut context, workspace.as_bytes());
    for entry in keys {
        update_framed_hmac(&mut context, entry.id.as_bytes());
        update_framed_hmac(&mut context, &entry.key[..]);
    }
    let tag = context.sign();
    let mut fingerprint = String::with_capacity(FINGERPRINT_PREFIX.len() + MAC_TEXT_CHARS);
    fingerprint.push_str(FINGERPRINT_PREFIX);
    URL_SAFE_NO_PAD.encode_string(tag, &mut fingerprint);
    fingerprint
}

fn hmac_context(identity_key: &[u8]) -> ring::hmac::Context {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, identity_key);
    ring::hmac::Context::with_key(&key)
}

fn envelope_aad(
    key_id: &[u8; KEY_ID_BYTES],
    nonce: &[u8; NONCE_BYTES],
    binding_digest: &[u8; BINDING_BYTES],
) -> [u8; AAD_BYTES] {
    let mut aad = [0u8; AAD_BYTES];
    let mut at = 0;
    aad[at..at + ENVELOPE_AAD_DOMAIN.len()].copy_from_slice(ENVELOPE_AAD_DOMAIN);
    at += ENVELOPE_AAD_DOMAIN.len();
    aad[at] = ENVELOPE_VERSION;
    at += 1;
    aad[at..at + KEY_ID_BYTES].copy_from_slice(key_id);
    at += KEY_ID_BYTES;
    aad[at..at + NONCE_BYTES].copy_from_slice(nonce);
    at += NONCE_BYTES;
    aad[at..at + BINDING_BYTES].copy_from_slice(binding_digest);
    aad
}

fn decode_envelope(token: &str, envelope: &mut [u8; ENVELOPE_BYTES]) -> Result<(), CursorError> {
    if token.len() != TOKEN_CHARS || !token.is_ascii() {
        return Err(CursorError);
    }
    let body = token.strip_prefix(TOKEN_PREFIX).ok_or(CursorError)?;
    let written = URL_SAFE_NO_PAD
        .decode_slice(body, envelope.as_mut_slice())
        .map_err(|_| CursorError)?;
    if written != ENVELOPE_BYTES {
        return Err(CursorError);
    }
    // Refuse non-canonical base64 spellings of the same bytes without
    // allocating a second encoded copy.
    let mut encoded = [0u8; TOKEN_BODY_CHARS];
    let encoded_len = URL_SAFE_NO_PAD
        .encode_slice(envelope.as_slice(), &mut encoded)
        .map_err(|_| CursorError)?;
    if encoded_len != body.len() || &encoded[..encoded_len] != body.as_bytes() {
        return Err(CursorError);
    }
    Ok(())
}

fn update_framed_digest(context: &mut ring::digest::Context, bytes: &[u8]) {
    context.update(&(bytes.len() as u32).to_be_bytes());
    context.update(bytes);
}

fn update_framed_hmac(context: &mut ring::hmac::Context, bytes: &[u8]) {
    context.update(&(bytes.len() as u32).to_be_bytes());
    context.update(bytes);
}

/// Bounded canonical text: nonempty when required, at most `max` bytes and
/// free of NUL/ASCII control bytes. Equivalent non-canonical spellings remain
/// distinct bindings, so callers must canonicalize filters before sealing.
fn canonical_text(value: &str, max: usize, allow_empty: bool) -> bool {
    if (!allow_empty && value.is_empty()) || value.len() > max {
        return false;
    }
    !value
        .bytes()
        .any(|byte| byte == 0 || byte < 0x20 || byte == 0x7f)
}

/// Bounded, nonempty-when-required text with no character restrictions.
/// Only the opaque principal uses this: the binding hashes its exact bytes
/// and the producer's length framing makes every byte sequence unambiguous, so
/// NUL/control bytes are data rather than a framing hazard. Domain, resource
/// and filter text keep the stricter [`canonical_text`].
fn bounded_text(value: &str, max: usize, allow_empty: bool) -> bool {
    (allow_empty || !value.is_empty()) && value.len() <= max
}

/// Vetted constant-time comparison from `ring`; no local byte loop.
#[allow(deprecated)]
fn constant_time_slices_equal(left: &[u8], right: &[u8]) -> bool {
    ring::constant_time::verify_slices_are_equal(left, right).is_ok()
}
