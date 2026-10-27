//! The shared v1 protected-Connection domain: strict DTOs, canonical material
//! and ledger digests, immutable admission/settlement rules, UID/role/provider
//! restrictions, exact integer pricing and known-secret projection.
//!
//! No filesystem, network, process, database, randomness or key-store owner
//! lives in this module. Trusted native/hosted adapters produce the admission
//! facts and perform the durable transaction; the rules here decide.
pub mod canonical;
pub mod dto;
pub mod ledger;
pub mod plan;
pub mod policy;
pub mod primitives;
pub mod schema;
pub mod secrets;

#[cfg(feature = "custody-crypto")]
pub mod crypto;

pub use canonical::{
    canonical_bytes, canonical_value, exact_byte_digest, material_digest,
    material_digest_for_domain, material_digest_value, parse_strict, ParseError,
    APP_MATERIAL_DOMAIN, CONNECTION_DISPATCH_DOMAIN, MAX_JSON_DEPTH, PROTECTED_LEDGER_DOMAIN,
};
pub use dto::*;
pub use ledger::*;
pub use plan::{
    plan_connection_dispatch_v1, quote_age_is_current, settle_connection_dispatch_v1,
    verify_frozen_dispatch_v1,
};
pub use policy::{pi_model_decision, role_allowlist, PiModelRefusalV1};
pub use primitives::*;
pub use secrets::{KnownSecretV1, ProjectionError};

#[cfg(feature = "custody-crypto")]
pub use crypto::{
    hosted_header_aad, hosted_record_aad, native_header_aad, native_record_aad,
    native_record_aad_borrowed, open_hosted_header, open_hosted_record, open_native_header,
    open_native_record, seal_hosted_header, seal_hosted_record, seal_native_header,
    seal_native_record, CryptoError, CustodyKey, HostedCustodyIdentityV1,
    NativeCustodyIdentityRefV1, NativeCustodyIdentityV1, HOSTED_HEADER_DOMAIN,
    HOSTED_RECORD_DOMAIN, NATIVE_HEADER_DOMAIN, NATIVE_RECORD_DOMAIN,
};
