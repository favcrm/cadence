//! Shared primitives used by the core agent and the downstream native daemon.

#[doc(hidden)]
pub mod app_team_state;
#[doc(hidden)]
pub mod console_port;
pub(crate) use console_port as protected_runtime;
#[doc(hidden)]
pub mod delivery_classifier;
#[cfg(all(debug_assertions, feature = "test-seam"))]
pub use delivery_classifier::queue_authority_acceptance;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[path = "installer_client.rs"]
mod installer_client;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[path = "installer_enrolled.rs"]
pub(crate) mod installer_enrolled;
mod installer_enrollment_wire;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) use installer_enrollment_wire::InstallerRecord as InstallerProcessRecord;
#[doc(hidden)]
pub mod monitor_scope;
#[cfg(test)]
pub(crate) use screen_frame::render_package_frame_html;
pub(crate) use screen_frame::{render_frame_html, SCREEN_KIT_CSS, SCREEN_KIT_VERSION};
#[doc(hidden)]
pub mod scoped_cursor;
#[doc(hidden)]
pub mod scoped_cursor_config;
#[doc(hidden)]
pub mod screen_frame;
#[doc(hidden)]
pub mod social_effect_id;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[doc(hidden)]
pub mod supervisor_grant;
