use super::*;

mod agent_gc_timer;
mod app_hint_tests;
mod app_unknown_fence_privacy;
mod auto_stop_timer;
mod cad1210_pm_dir_guard;
#[cfg(feature = "test-seam")]
mod cad1922_daemon_workspace_refusal;
#[cfg(feature = "test-seam")]
mod cad1989_group_root_profile_read;
#[cfg(feature = "test-seam")]
mod cad1989_profile_context_refusal;
mod cad1989_profile_create_refusal;
mod pty_retry_tests;
mod unit;
mod unknown_fence_guidance;
