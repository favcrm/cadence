//! Shared Cadence domain rules. No filesystem, process, network or database owner lives here.
pub mod app_compat;
pub mod board_api;
pub mod connection_dispatch;
pub mod contracts;
pub mod delivery;
pub mod delivery_requirements;
pub mod delivery_rules;
pub mod error;
pub mod evaluate;
pub mod frontmatter;
pub mod issue;
pub use error::{Error, Result};
pub use evaluate::evaluate;
pub use frontmatter::split_front;
// CAD-1085: the extracted canonical material digest is a drop-in for the
// historical `crate::store::app_runs::material_digest`.
pub use connection_dispatch::{material_digest, material_digest_for_domain};

#[cfg(all(target_arch = "wasm32", feature = "wasm-abi"))]
mod wasm;

// CAD-1085: the custody envelope primitives exist only in the separate
// custody build, so the ordinary `core.wasm` artifact carries no crypto entry
// point at all.
#[cfg(all(
    target_arch = "wasm32",
    feature = "wasm-abi",
    feature = "custody-crypto"
))]
#[path = "connection_dispatch/custody_wasm.rs"]
mod custody_wasm;

#[cfg(test)]
mod contract_tests;
