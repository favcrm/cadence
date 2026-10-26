//! Shared Cadence domain rules. No filesystem, process, network or database owner lives here.
pub mod app_compat;
pub mod board_api;
pub mod contracts;
pub mod error;
pub mod evaluate;
pub mod frontmatter;
pub mod issue;
pub mod material;
pub mod works;
pub use error::{Error, Result};
pub use evaluate::evaluate;
pub use frontmatter::split_front;

#[cfg(all(target_arch = "wasm32", feature = "wasm-abi"))]
mod wasm;

#[cfg(test)]
mod contract_tests;
