//! Native daemon: Unix socket dispatch, actor lifetime, and background workers.

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

// The daemon shares the agent's state and domain types; it does not compile a
// second copy of them. Keep existing crate-relative source paths at the mount.
#[cfg(not(test))]
use cadence_agent::ui;
use cadence_agent::{
    adapter, agent_roles, agent_settings, agent_uid, app_assistant, attachment_image,
    board_identity, business_templates, cli_actor, client, cloud_agent, confine, context_pack,
    continuity, contract_fixture, delivery, doctor, error, fd_fs, gh_guard, home, inbox, issue,
    issuer_transport, lease, managed_section, master, master_perm, memory, model_defaults,
    needs_dismiss, peer, pi_policy, proc, project_policy, protected_runtime, proto, reaper,
    remote_cli, rollout, runner, sandbox, secret, skill, slots, store, test_seam, thread_search,
    wiki, worktree, Error, Result,
};
#[cfg(all(test, feature = "test-seam"))]
use cadence_agent::{web_publish, web_sites};
#[cfg(all(test, feature = "test-seam"))]
use cadence_ops::briefing;
use cadence_ops::{
    assistant_actor, audit, backup, board_membership, business_export, delegation, device_login,
    devin_catalog, local_assignment, notify_bridge, notify_host, overview, review, test_queue,
    update, web_push,
};

#[path = "../../../src/operator_auth.rs"]
mod operator_auth;

mod platform {
    pub(crate) use crate::daemon::custody_rpc::{credential_enrollment_missing, load_credential};
    pub(crate) use cadence_agent::platform::*;
}

#[cfg(test)]
#[path = "../../../src/board_schema_test.rs"]
mod board_schema_test;
#[cfg(test)]
#[path = "../../../src/daemon/board_session_schema.rs"]
mod board_session_schema;

#[path = "../../../src/daemon.rs"]
pub mod daemon;
pub use daemon::*;

#[path = "../../../src/thread_budget.rs"]
mod thread_budget;

#[cfg(all(test, feature = "test-seam"))]
#[path = "../../../src/web_cli_test_harness.rs"]
mod web_cli_test_harness;

#[cfg(all(test, debug_assertions, feature = "test-seam"))]
#[path = "../../../src/backup/cad2215_portable_cursor_acceptance.rs"]
mod cad2215_portable_cursor_acceptance;

// The daemon's existing HTTP refusal fixtures exercise the real board.
// This is a dev-dependency only; service startup never depends on cadence-ui.
#[cfg(test)]
mod ui {
    pub(crate) use cadence_agent::ui::*;
    #[cfg(feature = "test-seam")]
    pub(crate) use cadence_ui::ServeOpts;

    #[cfg(feature = "test-seam")]
    pub(crate) fn serve(
        state_dir: &std::path::Path,
        pm_dir: &std::path::Path,
        opts: &ServeOpts,
    ) -> crate::Result<()> {
        cadence_ui::serve(state_dir, pm_dir, opts)
            .map_err(|err| crate::Error::internal(err.to_string()))
    }
}
