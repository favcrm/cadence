//! Downstream operational and workflow tier: audit, briefing,
//! delegation, local assignment, overview, review, setup, update/upgrade.

#![cfg_attr(test, allow(clippy::disallowed_methods))]
#![cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code, unused_imports)
)]

#[cfg(not(test))]
use cadence_agent::output as console_output;
#[cfg(not(test))]
#[macro_use]
#[path = "../../../src/console_macros.rs"]
mod console_macros;

#[cfg(test)]
extern crate self as cadence_ops;

#[cfg(test)]
#[doc(hidden)]
pub use cadence_agent::contract_fixture;
#[cfg(all(test, feature = "cli-runtime-test"))]
pub use cadence_agent::send_row_gate_take;
use cadence_agent::{
    adapter, agent_uid, backup, board_identity, business_templates, client, cloud_agent, confine,
    daemon, delivery, doctor, error, fd_fs, gh_guard, home, ident, inbox, issue, issuer_transport,
    lease, master, master_perm, memory, needs_dismiss, operator_auth, output, pi_policy, platform,
    printed_command, proc, proto, reaper, remote_auth, remote_cli, remote_enrollment, rollout,
    sandbox, secret, skill, slots, store, test_seam, thread_search, ui, wiki, worktree,
    WORKSPACE_MANIFEST_DIR,
};

#[path = "../../../src/assistant_actor.rs"]
pub mod assistant_actor;
#[path = "../../../src/audit/mod.rs"]
pub mod audit;
#[path = "../../../src/board_membership.rs"]
pub mod board_membership;
#[path = "../../../src/briefing.rs"]
pub mod briefing;
#[path = "../../../src/business_export.rs"]
pub mod business_export;
#[path = "../../../src/delegation.rs"]
pub mod delegation;
#[path = "../../../src/device_login.rs"]
pub mod device_login;
#[path = "../../../src/devin_catalog.rs"]
pub mod devin_catalog;
#[path = "../../../src/github_connect.rs"]
pub mod github_connect;
#[path = "../../../src/hosted_app_cli.rs"]
pub mod hosted_app_cli;
#[path = "../../../src/local_assignment.rs"]
pub mod local_assignment;
#[path = "../../../src/mcp.rs"]
pub mod mcp;
#[path = "../../../src/mcp_agent.rs"]
pub mod mcp_agent;
#[path = "../../../src/notify_bridge.rs"]
pub mod notify_bridge;
#[path = "../../../src/notify_host.rs"]
pub mod notify_host;
#[path = "../../../src/overview.rs"]
pub mod overview;
#[path = "../../../src/private_app_update.rs"]
pub mod private_app_update;
#[path = "../../../src/releases.rs"]
pub mod releases;
#[path = "../../../src/remote_migration.rs"]
pub mod remote_migration;
#[path = "../../../src/review.rs"]
pub mod review;
#[path = "../../../src/session.rs"]
pub mod session;
#[path = "../../../src/setup.rs"]
pub mod setup;
#[path = "../../../src/test_queue.rs"]
pub mod test_queue;
#[path = "../../../src/update.rs"]
pub mod update;
#[path = "../../../src/upgrade.rs"]
pub mod upgrade;
#[path = "../../../src/web_push.rs"]
pub mod web_push;
