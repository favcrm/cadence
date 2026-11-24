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
use cadence_agent::{
    adapter, agent_roles, agent_settings, agent_uid, app_assistant, assistant_actor,
    attachment_image, audit, backup, board_identity, board_membership, business_export,
    business_templates, cli_actor, client, cloud_agent, confine, context_pack, continuity,
    contract_fixture, delegation, delivery, device_login, devin_catalog, doctor, error, fd_fs,
    gh_guard, home, inbox, issue, issuer_transport, lease, local_assignment, managed_section, master,
    master_perm, memory, model_defaults, needs_dismiss, notify_bridge, notify_host, overview,
    peer, pi_policy, proc, project_policy, protected_runtime, proto, reaper, remote_cli,
    review, rollout, runner, sandbox, secret, skill, slots, store, test_queue, test_seam,
    thread_search, update, web_push, wiki, worktree, Error, Result,
};
#[cfg(test)]
use cadence_agent::{briefing, web_publish};
#[cfg(not(test))]
use cadence_agent::ui;

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

#[path = "../../../src/daemon/crate_root.rs"]
pub mod daemon;
pub use daemon::*;

#[path = "../../../src/thread_budget.rs"]
mod thread_budget;

#[cfg(test)]
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
    pub(crate) use cadence_ui::ServeOpts;

    pub(crate) fn serve(
        state_dir: &std::path::Path,
        pm_dir: &std::path::Path,
        opts: &ServeOpts,
    ) -> crate::Result<()> {
        cadence_ui::serve(state_dir, pm_dir, opts)
            .map_err(|err| crate::Error::internal(err.to_string()))
    }
}
