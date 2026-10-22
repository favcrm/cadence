//! Shared Cadence domain rules. No filesystem, process, network or database owner lives here.
pub mod app_compat;
pub mod command;
pub mod error;
pub use error::{Error, Result};

#[cfg(all(target_arch = "wasm32", feature = "wasm-abi"))]
mod wasm;
