//! Cadence agent controller — library surface.
//!
//! The CLI binary `cadence` is a thin client over this library plus the
//! Unix-socket daemon defined in [`daemon`].

// Test code spawns freely: a test binary never runs the CAD-308 reaper
// (only `daemon run` does, and `reaper`'s own proof in a process of its
// own), so the spawn registry need not see test spawns.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

pub mod adapter;
pub mod audit;
pub mod backup;
pub mod client;
pub mod daemon;
pub mod delivery;
pub mod doctor;
pub mod error;
pub mod inbox;
pub mod issue;
pub mod master;
pub mod mcp;
pub mod memory;
pub mod model_defaults;
pub mod overview;
pub mod peer;
pub mod proc;
pub mod proto;
pub mod reaper;
pub mod review;
pub mod rollout;
pub mod runner;
pub mod sandbox;
pub mod secret;
pub mod session;
pub mod setup;
pub mod skill;
pub mod slots;
pub mod store;
pub mod tailnet_proof;
pub mod ui;
pub mod upgrade;
pub mod worktree;

pub use error::{Error, Result};
