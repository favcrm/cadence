//! Shared primitives used by the core agent and the downstream native daemon.

#[doc(hidden)]
#[path = "daemon/app_team_state.rs"]
pub mod app_team_state;
#[doc(hidden)]
#[path = "daemon/console_port.rs"]
pub mod console_port;
pub(crate) use console_port as protected_runtime;
#[doc(hidden)]
#[path = "daemon/delivery_classifier.rs"]
pub mod delivery_classifier;
#[cfg(all(debug_assertions, feature = "test-seam"))]
pub use delivery_classifier::queue_authority_acceptance;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[path = "daemon/installer_client.rs"]
mod installer_client;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[path = "daemon/installer_enrolled.rs"]
pub(crate) mod installer_enrolled;
#[path = "daemon/installer_enrollment_wire.rs"]
mod installer_enrollment_wire;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) use installer_enrollment_wire::InstallerRecord as InstallerProcessRecord;
#[doc(hidden)]
#[path = "daemon/monitor_scope.rs"]
pub mod monitor_scope;
pub(crate) use screen_frame::{render_frame_html, SCREEN_KIT_CSS, SCREEN_KIT_VERSION};
#[cfg(test)]
pub(crate) use screen_frame::render_package_frame_html;
#[doc(hidden)]
#[path = "daemon/screen_frame.rs"]
pub mod screen_frame;
#[doc(hidden)]
#[path = "daemon/scoped_cursor.rs"]
pub mod scoped_cursor;
#[doc(hidden)]
#[path = "daemon/scoped_cursor_config.rs"]
pub mod scoped_cursor_config;
#[doc(hidden)]
#[path = "daemon/social_effect_id.rs"]
pub mod social_effect_id;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[doc(hidden)]
#[path = "daemon/supervisor_grant.rs"]
pub mod supervisor_grant;
