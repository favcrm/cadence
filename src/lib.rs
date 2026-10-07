//! Cadence agent controller — library surface.
//!
//! The CLI binary `cadence` is a thin client over this library plus the
//! Unix-socket daemon defined in [`daemon`].

// Test code spawns freely: a test binary never runs the CAD-308 reaper
// (only `daemon run` does, and `reaper`'s own proof in a process of its
// own), so the spawn registry need not see test spawns.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

pub mod adapter;
pub mod agent_uid;
pub mod app_assistant;
pub mod audit;
pub mod backup;
pub mod board_identity;
pub mod cli_actor;
pub mod client;
pub mod confine;
pub mod continuity;
// Test support for the shared ADR 0006 fixture — a deterministic
// platform double, not a live adapter. Hidden from the public API docs.
#[doc(hidden)]
pub mod contract_fixture;
pub mod daemon;
pub mod delegation;
pub mod delivery;
// CAD-777: board sign-in through the AgenticOS device grant.
pub mod device_login;
pub mod devin_catalog;
pub mod doctor;
pub mod error;
pub mod filter;
pub mod home;
pub mod inbox;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) mod installer_bundle;
pub mod issue;
pub mod lease;
pub mod master;
pub mod master_perm;
pub mod mcp;
pub mod mcp_agent;
pub mod memory;
pub mod model_defaults;
pub mod needs_dismiss;
pub mod operator_auth;
pub mod output;
pub mod overview;
pub mod peer;
pub mod pi_policy;
pub mod platform;
pub mod proc;
#[allow(dead_code)] // Shared helper grammar; production prerequisites remain unavailable.
pub(crate) mod protected_pi_profile;
pub mod proto;
pub mod reaper;
pub mod remote_auth;
pub mod remote_cli;
pub mod remote_enrollment;
pub mod remote_result_outbox;
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
pub mod test_queue;
pub mod test_seam;
pub mod ui;
pub mod update;
pub mod upgrade;
pub mod wiki;
pub mod worktree;

pub use error::{Error, Result};

/// Fixed no-argument non-setuid carrier entry, never a configurable exec API.
pub fn installer_carrier_entry() -> Result<()> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        installer_bundle::carrier_entry()
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        Err(Error::rejected("installer bundle requires Linux x86_64"))
    }
}
/// Fixed host-stdin waiting entry; construction is not enrollment/release.
pub fn installer_waiting_client_entry() -> Result<()> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        installer_bundle::client_entry()
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        Err(Error::rejected("installer bundle requires Linux x86_64"))
    }
}
/// Fixed host-only observer entry; bounded observations never elect authority.
pub fn installer_observer_entry() -> Result<()> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        installer_bundle::observer_entry()
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        Err(Error::rejected("installer bundle requires Linux x86_64"))
    }
}
