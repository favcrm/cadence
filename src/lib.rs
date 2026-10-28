//! Cadence agent controller — library surface.
//!
//! The CLI binary `cadence` is a thin client over this library plus the
//! Unix-socket daemon defined in [`daemon`].

// Test code spawns freely: a test binary never runs the CAD-308 reaper
// (only `daemon run` does, and `reaper`'s own proof in a process of its
// own), so the spawn registry need not see test spawns.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

pub mod adapter;
pub mod agent_roles;
pub mod agent_settings;
pub mod agent_uid;
pub mod app_assistant;
pub mod app_preview_export;
pub mod assistant_actor;
pub mod audit;
pub mod backup;
pub mod board_identity;
pub mod board_membership;
pub mod briefing;
pub mod business_export;
pub mod business_templates;
pub mod cli_actor;
pub mod client;
pub mod cloud_agent;
pub mod confine;
pub mod context_pack;
pub mod continuity;
pub mod hosted_app_cli;
pub mod notify_bridge;
pub mod notify_host;
pub mod web_push;
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
pub mod gh_guard;
pub mod home;
mod ident;
pub mod inbox;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) mod installer_bundle;
pub mod issue;
#[cfg(test)]
mod issuer_tls_fixture;
pub(crate) mod issuer_transport;
pub mod lease;
pub mod local_assignment;
pub(crate) mod managed_section;
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
pub(crate) mod protected_pi_profile;
pub mod protected_runtime;
pub mod proto;
pub mod reaper;
pub mod releases;
pub mod remote_auth;
pub mod remote_cli;
pub mod remote_enrollment;
pub mod remote_migration;
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
#[cfg(test)]
pub(crate) mod test_ports;
pub mod test_queue;
pub mod test_seam;
pub(crate) mod thread_budget;
pub mod ui;
pub mod update;
pub mod upgrade;
pub mod web_publish;
pub mod web_sites;
pub mod wiki;
pub mod worktree;

// CAD-1429: test-only harness that compiles the production `cli/web.rs`
// into this library's unit tests (so the loopback test pin applies). Absent
// from every non-test build.
#[cfg(test)]
extern crate self as cadence_agent;
#[cfg(test)]
mod cad1085_entry_authority_acceptance;
#[cfg(test)]
mod test_web_issuer_tls;
#[cfg(test)]
mod web_cli_test_harness;

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
