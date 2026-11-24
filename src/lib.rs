//! Cadence agent controller — library surface.
//!
//! The CLI binary `cadence` is a thin client over this library plus the
//! Unix-socket daemon defined in [`daemon`].

// Test code spawns freely: a test binary never runs the CAD-308 reaper
// (only `daemon run` does, and `reaper`'s own proof in a process of its
// own), so the spawn registry need not see test spawns.
#![cfg_attr(test, allow(clippy::disallowed_methods))]
// CAD-1085: the protected runtime and its whole daemon surface exist only on
// x86_64 Linux. Other targets compile the shared types so cross-platform code
// keeps building, and the protected paths are dormant by design, so their
// helpers read as dead code there. Keep the lint fully on where it matters.
#![cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code, unused_imports)
)]

// Production console output must not interrupt a finite operation on EPIPE.
// Tests retain libtest's normal output capture; CLI integration tests run the
// production writers.
#[cfg(not(test))]
use crate::output as console_output;
#[cfg(not(test))]
#[macro_use]
mod console_macros;

pub mod adapter;
pub mod agent_roles;
pub mod agent_settings;
pub mod agent_uid;
pub mod app_assistant;
pub mod app_preview_export;
pub mod attachment_image;
/// Approval evidence constant shared with [`delivery::hosted_gate`].
pub mod audit {
    #[doc(hidden)]
    pub const APPROVAL_UNVERIFIED: &str = "unverified until CAD-280";
}
pub mod board_identity;
/// Kickoff guidance constant shared with [`issue::dispatch::kickoff`] and [`store::kickoff`].
pub mod briefing {
    #[doc(hidden)]
    pub const TASK_GUIDANCE: &str = " Read the Cadence protocol in \
        $HOME/.agents/skills/cadence/SKILL.md (Business knowledge for company facts). \
        If missing, use `cadence skill show cadence --file SKILL.md` where allowed. \
        Report unreadable required SOPs before the affected step.";
}
pub mod business_templates;
pub mod cli_actor;
pub mod client;
pub mod cloud_agent;
pub mod confine;
pub mod context_pack;
pub mod continuity;
// Test support for the shared ADR 0006 fixture — a deterministic
// platform double, not a live adapter. Hidden from the public API docs.
#[doc(hidden)]
pub mod contract_fixture;
#[path = "daemon/shared.rs"]
pub mod daemon;
/// Digest and scope action constants shared by core storage, issue and worktree seams.
pub mod delegation {
    use sha2::{Digest, Sha256};

    /// The approval action of a ticket-time scope pre-approval.
    pub const SCOPE_ACTION: &str = "scope";

    pub fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}
pub mod delivery;
// CAD-777: board sign-in through the AgenticOS device grant.
#[cfg(test)]
mod board_schema_test;
/// Device-session lifetime limits shared with [`operator_auth`].
pub mod device_login {
    /// Remote (device-grant) board sessions live shorter than on-host ones:
    /// 12 h idle, 24 h absolute. See `open_device`.
    pub const REMOTE_IDLE_SECS: i64 = 12 * 3600;
    pub const REMOTE_ABSOLUTE_SECS: i64 = 24 * 3600;
}
pub mod doctor;
pub mod error;
pub mod fd_fs;
pub mod filter;
pub mod gh_guard;
pub mod home;
#[doc(hidden)]
pub mod ident;
pub mod inbox;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[doc(hidden)]
pub mod installer_bundle;
pub mod issue;
#[doc(hidden)]
pub mod issuer_transport;
pub mod lease;
#[doc(hidden)]
pub mod managed_section;
pub mod master;
pub mod master_perm;
/// Static and deterministic generated MCP tool names shared with [`adapter::codex`] and `cadence-ops`.
pub mod mcp_agent {
    use sha2::{Digest, Sha256};

    #[doc(hidden)]
    pub const STATIC_TOOL_NAMES: [&str; 16] = [
        "self",
        "wiki_search",
        "wiki_read",
        "issue_show",
        "memory_match",
        "memory_propose",
        "thread_search",
        "app_run_artifact",
        "private_app_prepare",
        "private_app_status",
        "private_app_apply",
        "private_app_reconcile",
        "app_describe",
        "app_skill",
        "app_call",
        "board_pending_actions",
    ];

    #[doc(hidden)]
    pub fn mcp_name_part(value: &str) -> String {
        value
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    #[doc(hidden)]
    pub fn static_tool_names() -> &'static [&'static str] {
        &STATIC_TOOL_NAMES
    }

    /// Generated MCP names for one reviewed inventory, in one deterministic
    /// order for every consumer.
    #[doc(hidden)]
    pub fn generated_tool_names<'a>(
        entries: impl Iterator<Item = (&'a str, &'a str, bool)>,
    ) -> Vec<(String, String, String)> {
        let mut used: std::collections::HashSet<String> = STATIC_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        let mut out = Vec::new();
        for (platform, tool, direct) in entries {
            if !direct {
                continue;
            }
            let name = platform_tool_name(platform, tool, &mut used);
            out.push((name, platform.to_string(), tool.to_string()));
        }
        out
    }

    fn platform_tool_name(
        platform: &str,
        tool: &str,
        used: &mut std::collections::HashSet<String>,
    ) -> String {
        let base = format!("{}_{}", mcp_name_part(platform), mcp_name_part(tool));
        if used.insert(base.clone()) {
            return base;
        }
        let mut digest = Sha256::new();
        digest.update(platform.as_bytes());
        digest.update(b"\0");
        digest.update(tool.as_bytes());
        let hex = format!("{:x}", digest.finalize());
        let mut attempt = 0u32;
        loop {
            let suffix = match attempt {
                0 => format!("_{}", &hex[..6]),
                attempt => format!("_{}_{attempt}", &hex[..6]),
            };
            let room = 64usize.saturating_sub(suffix.len());
            let stem: String = base.chars().take(room).collect();
            let candidate = format!("{stem}{suffix}");
            if used.insert(candidate.clone()) {
                return candidate;
            }
            attempt = attempt.saturating_add(1);
        }
    }
}
pub mod memory;
pub mod model_defaults;
pub mod needs_dismiss;
#[path = "operator_auth_shared.rs"]
pub mod operator_auth;
pub mod output;
/// Build identity constants and check-rollup predicate shared across core, ops, daemon, ui and cli.
pub mod overview {
    use serde_json::Value;

    /// Git identity baked in by build.rs — `unknown` when git or a repo
    /// was absent at build time, which every consumer reads as "cannot
    /// tell", never as zero.
    pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
    pub const BUILD_COMMIT: &str = env!("CADENCE_BUILD_COMMIT");
    /// The full build id `cadence --version` prints (`0.1.0+<sha>`, the
    /// same concat main.rs gives clap).
    pub const BUILD_ID: &str =
        concat!(env!("CARGO_PKG_VERSION"), "+", env!("CADENCE_BUILD_COMMIT"));
    pub const BUILD_TIME: &str = env!("CADENCE_BUILD_TIME");
    pub const BUILD_REMOTE: &str = env!("CADENCE_BUILD_REMOTE");
    pub const BUILD_ROOT: &str = env!("CADENCE_BUILD_ROOT");

    pub fn checks_green_pub(rollup: &[Value]) -> bool {
        rollup.iter().all(|e| {
            let name = e["context"].as_str().or(e["name"].as_str()).unwrap_or("");
            if name.eq_ignore_ascii_case("qa-verdict") {
                return true;
            }
            if e["__typename"].as_str() == Some("CheckRun") || e["conclusion"].is_string() {
                let done = e["status"].as_str() == Some("COMPLETED") || e["status"].is_null();
                let ok = matches!(
                    e["conclusion"].as_str(),
                    Some("SUCCESS" | "SKIPPED" | "NEUTRAL")
                );
                return done && ok;
            }
            e["state"].as_str() == Some("SUCCESS")
        })
    }
}
pub mod peer;
pub mod pi_policy;
pub mod platform;
#[doc(hidden)]
pub mod printed_command;
pub mod proc;
#[doc(hidden)]
pub mod project_policy;
pub(crate) mod protected_pi_profile;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod protected_runtime;
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
#[path = "protected_runtime_stub.rs"]
pub mod protected_runtime;
pub mod proto;
pub mod reaper;
pub mod remote_auth;
pub mod remote_cli;
pub mod remote_enrollment;
pub mod remote_result_outbox;
pub mod rollout;
pub mod runner;
pub mod sandbox;
pub mod secret;
pub mod skill;
pub mod slots;
pub mod store;
pub mod tailnet_proof;
#[cfg(test)]
pub(crate) mod test_ports;
pub mod test_seam;
#[cfg(feature = "test-seam")]
#[doc(hidden)]
pub fn send_row_gate_take(gate: &test_seam::SendRowGate) {
    gate.take();
}
pub mod thread_search;
pub mod ui;
/// Release path parsing shared with [`master::confine`] and `cadence-ops::upgrade`.
pub mod upgrade {
    use std::path::{Path, PathBuf};

    pub const BINARY: &str = "cadence";

    /// A full lowercase 40-hex commit id — never an abbreviation, a ref
    /// name, or anything a shell or `gh` could read as an option.
    pub fn is_full_sha(s: &str) -> bool {
        s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }

    /// `<dir>/<40-hex>/cadence` → `(<dir>, sha)`.
    #[doc(hidden)]
    pub fn release_of(target: &Path) -> Option<(PathBuf, String)> {
        if target.file_name()? != BINARY {
            return None;
        }
        let sha_dir = target.parent()?;
        let sha = sha_dir.file_name()?.to_str()?;
        if !is_full_sha(sha) {
            return None;
        }
        Some((sha_dir.parent()?.to_path_buf(), sha.to_string()))
    }

    /// A release tag as `install.sh` names its directory: `v` and a version
    /// made of ASCII alphanumerics and `.+_-`, starting with a digit.
    pub fn is_version_tag(s: &str) -> bool {
        s.strip_prefix('v').is_some_and(|v| {
            v.starts_with(|c: char| c.is_ascii_digit())
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-'))
        })
    }

    /// The releases dir holding a link target: `<dir>/<40-hex>/cadence` (an
    /// upgrade) or `<dir>/v<version>/cadence` (an `install.sh` release).
    #[doc(hidden)]
    pub fn releases_dir_of(target: &Path) -> Option<PathBuf> {
        if let Some((dir, _)) = release_of(target) {
            return Some(dir);
        }
        if target.file_name()? != BINARY {
            return None;
        }
        let tag_dir = target.parent()?;
        if !is_version_tag(tag_dir.file_name()?.to_str()?) {
            return None;
        }
        Some(tag_dir.parent()?.to_path_buf())
    }
}
pub mod web_publish;
pub mod web_sites;
pub mod wiki;
pub mod worktree;

// CAD-1429: test-only harness that compiles the production `cli/web.rs`
// into this library's unit tests (so the loopback test pin applies). Absent
// from every non-test build.
#[cfg(test)]
extern crate self as cadence_agent;
#[cfg(any(test, feature = "test-seam"))]
#[doc(hidden)]
pub mod test_web_issuer_tls;

pub use error::{Error, Result};

/// Shipped native host version, shared by workspace runtime crates.
pub const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Root manifest path for workspace-relative converter assets.
#[doc(hidden)]
pub const WORKSPACE_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

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
