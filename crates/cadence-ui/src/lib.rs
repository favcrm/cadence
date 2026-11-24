//! HTTP board UI server (`tiny_http`), REST/SSE routes, and browser/HTTP acceptance tests.

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

#[path = "../../../src/ui/crate_root.rs"]
pub mod ui;
pub use ui::*;

#[cfg(all(test, feature = "test-seam", debug_assertions))]
#[path = "../../../src/ui/hosted_source_board_fixture.rs"]
mod hosted_source_board_fixture;

#[cfg(feature = "test-seam")]
pub use cadence_agent::test_web_issuer_tls;
pub use cadence_agent::{
    adapter, agent_roles, agent_uid, assistant_actor, board_identity, briefing, cli_actor, client,
    delivery, device_login, doctor, error, filter, gh_guard, ident, issue, issuer_transport, lease,
    master, memory, model_defaults, needs_dismiss, operator_auth, overview, peer, platform,
    project_policy, proto, reaper, rollout, secret, setup, skill, store, tailnet_proof, test_seam,
    update, upgrade, wiki, worktree, Error, Result,
};
pub use cadence_daemon::daemon;
