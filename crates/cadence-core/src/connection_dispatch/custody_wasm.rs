//! Raw-buffer custody envelope exports.
//!
//! This module is linked only by the custody build
//! (`--features wasm-abi,custody-crypto`); the ordinary `core.wasm` artifact
//! contains no `core_custody_*` symbol. No key or plaintext is ever returned
//! except as an explicitly requested sealed/desealed buffer, and the caller
//! owns and must zeroize/free its own linear memory.
//!
//! Buffer protocol: each entry returns `u64::MAX` on refusal, otherwise
//! `(length << 32) | pointer` exactly like `core_evaluate`. Header seal returns
//! a tag buffer; header open returns `1` on success.
//!
//! `kind` is `1` for native custody (byte-identical AAD to
//! `src/platform/encrypted_sqlite.rs`) and `2` for the hosted
//! `tenant-custody-worker/1` domain.
//!
//! Identity revisions and epochs use canonical positive decimal JSON strings,
//! bounded by `i64::MAX`; JSON numbers are refused. Native record AAD retains
//! its existing big-endian `u64` byte representation.
//!
//! JavaScript passes direct `u64` arguments as `bigint`. Normalize returned
//! values with `BigInt.asUintN(64, value)` before checking the refusal sentinel:
//! a WebAssembly `i64` result exposes `u64::MAX` as `-1n`. Check packed pointer
//! and length bounds, then zeroize secret inputs and outputs before freeing.
use crate::connection_dispatch::canonical::parse_strict;
use crate::connection_dispatch::crypto::{
    hosted_record_aad, native_record_aad, open_hosted_header, open_hosted_record,
    open_native_header, open_native_record, seal_hosted_header, seal_hosted_record,
    seal_native_header, seal_native_record, CustodyKey, HostedCustodyIdentityV1,
    NativeCustodyIdentityV1,
};

const IDENTITY_MAX_BYTES: usize = 4_096;
const KIND_NATIVE: u32 = 1;
const KIND_HOSTED: u32 = 2;
const FAILED: u64 = u64::MAX;
const SUCCESS: u64 = 1;

enum Identity {
    Native(NativeCustodyIdentityV1),
    Hosted(HostedCustodyIdentityV1),
}

fn export(bytes: Vec<u8>) -> u64 {
    let boxed = bytes.into_boxed_slice();
    let length = boxed.len() as u64;
    let pointer = Box::into_raw(boxed) as *mut u8 as u64;
    (length << 32) | pointer
}

unsafe fn slice<'a>(pointer: *const u8, length: usize) -> Option<&'a [u8]> {
    if pointer.is_null() {
        return None;
    }
    Some(std::slice::from_raw_parts(pointer, length))
}

unsafe fn fixed<const N: usize>(pointer: *const u8) -> Option<[u8; N]> {
    slice(pointer, N).and_then(|bytes| bytes.try_into().ok())
}

unsafe fn key(pointer: *const u8) -> Option<CustodyKey> {
    slice(pointer, crate::connection_dispatch::crypto::KEY_BYTES)
        .and_then(|bytes| CustodyKey::from_bytes(bytes).ok())
}

fn identity(kind: u32, bytes: &[u8]) -> Option<Identity> {
    match kind {
        KIND_NATIVE => parse_strict::<NativeCustodyIdentityV1>(bytes, IDENTITY_MAX_BYTES)
            .ok()
            .map(Identity::Native),
        KIND_HOSTED => parse_strict::<HostedCustodyIdentityV1>(bytes, IDENTITY_MAX_BYTES)
            .ok()
            .map(Identity::Hosted),
        _ => None,
    }
}

fn workspace(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes).ok()
}

#[no_mangle]
pub unsafe extern "C" fn core_custody_record_aad_v1(
    kind: u32,
    identity_ptr: *const u8,
    identity_len: usize,
    locator_ptr: *const u8,
) -> u64 {
    let Some(identity_bytes) = slice(identity_ptr, identity_len) else {
        return FAILED;
    };
    let Some(locator) = fixed::<16>(locator_ptr) else {
        return FAILED;
    };
    match identity(kind, identity_bytes) {
        Some(Identity::Native(identity)) => export(native_record_aad(&identity, &locator)),
        Some(Identity::Hosted(identity)) => export(hosted_record_aad(&identity, &locator)),
        None => FAILED,
    }
}

#[no_mangle]
pub unsafe extern "C" fn core_custody_header_aad_v1(
    kind: u32,
    workspace_ptr: *const u8,
    workspace_len: usize,
    key_id_ptr: *const u8,
    custody_epoch: u64,
) -> u64 {
    let Some(workspace_bytes) = slice(workspace_ptr, workspace_len) else {
        return FAILED;
    };
    let Some(workspace) = workspace(workspace_bytes) else {
        return FAILED;
    };
    let Some(key_id) = fixed::<16>(key_id_ptr) else {
        return FAILED;
    };
    match kind {
        KIND_NATIVE => export(crate::connection_dispatch::crypto::native_header_aad(
            workspace, &key_id,
        )),
        KIND_HOSTED => export(crate::connection_dispatch::crypto::hosted_header_aad(
            workspace,
            &key_id,
            custody_epoch,
        )),
        _ => FAILED,
    }
}

#[allow(clippy::too_many_arguments)]
#[no_mangle]
pub unsafe extern "C" fn core_custody_seal_v1(
    kind: u32,
    identity_ptr: *const u8,
    identity_len: usize,
    locator_ptr: *const u8,
    key_ptr: *const u8,
    nonce_ptr: *const u8,
    plaintext_ptr: *const u8,
    plaintext_len: usize,
) -> u64 {
    let Some(identity_bytes) = slice(identity_ptr, identity_len) else {
        return FAILED;
    };
    let Some(locator) = fixed::<16>(locator_ptr) else {
        return FAILED;
    };
    let Some(key) = key(key_ptr) else {
        return FAILED;
    };
    let Some(nonce) = fixed::<24>(nonce_ptr) else {
        return FAILED;
    };
    let Some(plaintext) = slice(plaintext_ptr, plaintext_len) else {
        return FAILED;
    };
    let sealed = match identity(kind, identity_bytes) {
        Some(Identity::Native(identity)) => {
            seal_native_record(&key, &identity, &locator, &nonce, plaintext)
        }
        Some(Identity::Hosted(identity)) => {
            seal_hosted_record(&key, &identity, &locator, &nonce, plaintext)
        }
        None => return FAILED,
    };
    match sealed {
        Ok(bytes) => export(bytes),
        Err(_) => FAILED,
    }
}

#[allow(clippy::too_many_arguments)]
#[no_mangle]
pub unsafe extern "C" fn core_custody_open_v1(
    kind: u32,
    identity_ptr: *const u8,
    identity_len: usize,
    locator_ptr: *const u8,
    key_ptr: *const u8,
    nonce_ptr: *const u8,
    ciphertext_ptr: *const u8,
    ciphertext_len: usize,
) -> u64 {
    let Some(identity_bytes) = slice(identity_ptr, identity_len) else {
        return FAILED;
    };
    let Some(locator) = fixed::<16>(locator_ptr) else {
        return FAILED;
    };
    let Some(key) = key(key_ptr) else {
        return FAILED;
    };
    let Some(nonce) = fixed::<24>(nonce_ptr) else {
        return FAILED;
    };
    let Some(ciphertext) = slice(ciphertext_ptr, ciphertext_len) else {
        return FAILED;
    };
    let opened = match identity(kind, identity_bytes) {
        Some(Identity::Native(identity)) => {
            open_native_record(&key, &identity, &locator, &nonce, ciphertext)
        }
        Some(Identity::Hosted(identity)) => {
            open_hosted_record(&key, &identity, &locator, &nonce, ciphertext)
        }
        None => return FAILED,
    };
    match opened {
        // `Zeroizing<Vec<u8>>` is copied out for the trusted caller; the
        // local buffer is zeroized on drop.
        Ok(plaintext) => export(plaintext.to_vec()),
        Err(_) => FAILED,
    }
}

#[no_mangle]
pub unsafe extern "C" fn core_custody_seal_header_v1(
    kind: u32,
    workspace_ptr: *const u8,
    workspace_len: usize,
    key_id_ptr: *const u8,
    custody_epoch: u64,
    key_ptr: *const u8,
    nonce_ptr: *const u8,
) -> u64 {
    let Some(workspace_bytes) = slice(workspace_ptr, workspace_len) else {
        return FAILED;
    };
    let Some(workspace) = workspace(workspace_bytes) else {
        return FAILED;
    };
    let Some(key_id) = fixed::<16>(key_id_ptr) else {
        return FAILED;
    };
    let Some(key) = key(key_ptr) else {
        return FAILED;
    };
    let Some(nonce) = fixed::<24>(nonce_ptr) else {
        return FAILED;
    };
    let sealed = match kind {
        KIND_NATIVE => seal_native_header(&key, workspace, &key_id, &nonce),
        KIND_HOSTED => seal_hosted_header(&key, workspace, &key_id, custody_epoch, &nonce),
        _ => return FAILED,
    };
    match sealed {
        Ok(tag) => export(tag),
        Err(_) => FAILED,
    }
}

#[allow(clippy::too_many_arguments)]
#[no_mangle]
pub unsafe extern "C" fn core_custody_open_header_v1(
    kind: u32,
    workspace_ptr: *const u8,
    workspace_len: usize,
    key_id_ptr: *const u8,
    custody_epoch: u64,
    key_ptr: *const u8,
    nonce_ptr: *const u8,
    tag_ptr: *const u8,
) -> u64 {
    let Some(workspace_bytes) = slice(workspace_ptr, workspace_len) else {
        return FAILED;
    };
    let Some(workspace) = workspace(workspace_bytes) else {
        return FAILED;
    };
    let Some(key_id) = fixed::<16>(key_id_ptr) else {
        return FAILED;
    };
    let Some(key) = key(key_ptr) else {
        return FAILED;
    };
    let Some(nonce) = fixed::<24>(nonce_ptr) else {
        return FAILED;
    };
    let Some(tag) = slice(tag_ptr, crate::connection_dispatch::crypto::TAG_BYTES) else {
        return FAILED;
    };
    let opened = match kind {
        KIND_NATIVE => open_native_header(&key, workspace, &key_id, &nonce, tag),
        KIND_HOSTED => open_hosted_header(&key, workspace, &key_id, custody_epoch, &nonce, tag),
        _ => return FAILED,
    };
    match opened {
        Ok(()) => SUCCESS,
        Err(_) => FAILED,
    }
}
