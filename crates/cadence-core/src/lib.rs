//! Shared Cadence domain rules. No filesystem, process, network or database owner lives here.
pub mod app_compat;
pub mod error;
pub mod evaluate;
pub mod frontmatter;
pub use error::{Error, Result};
pub use evaluate::evaluate;
pub use frontmatter::split_front;

#[cfg(all(target_arch = "wasm32", feature = "wasm-abi"))]
mod wasm;
