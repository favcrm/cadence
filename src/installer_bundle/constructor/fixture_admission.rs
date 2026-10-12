//! CAD-1159 SEC-002 debug-only fixture image-authority adapter.
//!
//! This module exists ONLY in the separately labelled debug + test-seam
//! artifact (`all(debug_assertions, feature = "test-seam",
//! feature = "native-fixture-admission")`); the crate-level compile_error in
//! src/test_seam.rs refuses every other combination. It carries the PUBLIC
//! image-verification root the owner elected through the fixed private build
//! input `.cadence/native-fixture-public-root-v1.txt` — never a signer,
//! private key, caller input, env/argv selection or runtime path loader.
//!
//! Admission beyond a signature match is strict and purpose-specific: the
//! manifest's `source` must equal the owner-elected pin and the build-time
//! election window (`notBeforeMs`..`expiresAtMs`) must still contain `now`,
//! alongside the unmodified manifest `notBeforeMs`/`expiresAtMs` guards. A
//! signed attestation outside that scope refuses the same way an unknown
//! root does. `IMAGE_AUTHORITY_KEYS` and the seam-free release constructor
//! are byte-for-byte unchanged — they stay empty and refuse.

use super::*;

mod generated {
    // Safe, owner-elected PUBLIC data emitted by build.rs. The empty stub
    // produced for any non-fixture compilation can never authenticate: no
    // kid/version lookup succeeds against a zero-length slice, so the
    // existing "unconfigured or untrusted" refusal stands.
    include!(concat!(env!("OUT_DIR"), "/native_fixture_root.rs"));
}

/// The compiled fixture image-verification roots, in the fixed
/// `ImageAuthorityKey` shape the production lookup consumes.
fn fixture_image_authority_keys() -> Vec<ImageAuthorityKey> {
    generated::NATIVE_FIXTURE_IMAGE_AUTHORITY_KEYS
        .iter()
        .map(|key| ImageAuthorityKey {
            kid: key.kid,
            version: key.version,
            public_key: key.public_key,
        })
        .collect()
}

/// Image-authority candidates for this gated artifact: exactly the elected
/// fixture roots. The unchanged `IMAGE_AUTHORITY_KEYS` stays authoritative
/// for every build this module is absent from — here it is compiled in
/// alongside so a future same-kid production pin cannot be shadowed by the
/// fixture election (fixture entries are consulted last).
pub(super) fn image_authority_keys() -> Vec<ImageAuthorityKey> {
    let mut keys: Vec<ImageAuthorityKey> = IMAGE_AUTHORITY_KEYS
        .iter()
        .map(|key| ImageAuthorityKey {
            kid: key.kid,
            version: key.version,
            public_key: key.public_key,
        })
        .collect();
    keys.extend(fixture_image_authority_keys());
    keys
}

/// Owner-elected scope on an already signature-verified manifest. The
/// election admits only the exact pinned `source` inside the elected
/// window; everything else refuses before the manifest is returned or any
/// constructor effect runs.
pub(super) fn enforce_election_scope(
    key: &ImageAuthorityKey,
    manifest: &Manifest,
    now_ms: u64,
) -> Result<()> {
    let Some(election) = generated::NATIVE_FIXTURE_IMAGE_AUTHORITY_KEYS
        .iter()
        .find(|fixture| fixture.kid == key.kid && fixture.version == key.version)
    else {
        // Not a fixture-elected key: production election machinery alone
        // decided this lookup — this adapter imposes nothing.
        return Ok(());
    };
    if manifest.source != election.source
        || now_ms < election.not_before_ms
        || now_ms >= election.expires_at_ms
        || manifest.not_before_ms < election.not_before_ms
        || manifest.expires_at_ms > election.expires_at_ms
    {
        return Err(refused());
    }
    Ok(())
}
