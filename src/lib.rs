//! Cadence agent controller — library surface.
//!
//! The CLI binary `cadence` is a thin client over this library plus the
//! Unix-socket daemon defined in [`daemon`].

pub mod adapter;
pub mod client;
pub mod daemon;
pub mod doctor;
pub mod error;
pub mod issue;
pub mod proto;
pub mod skill;
pub mod store;
pub mod ui;

pub use error::{Error, Result};
