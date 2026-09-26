//! CAD-535: `cadence agent-uid` — moved verbatim from src/main.rs. The verb dispatches in
//! `run()` before state-dir resolution, so its arm stays there; this file carries its items.

use clap::Subcommand;
use std::path::PathBuf;

/// `cadence agent-uid …` — ADR 0007 T1. Each verb is read-only except
/// `provision`, and `provision` refuses to run unless it is genuinely
/// root.
#[derive(Subcommand)]
pub(crate) enum AgentUidAction {
    /// Make the host match §5 — users, groups, `/var/lib/cadence`,
    /// `/opt/cadence`, the §3 modes and the setuid helper — checking
    /// before every step so a second run is a no-op. §4's negative
    /// assertions run as a pre-flight and refuse the run. Exit 2 on
    /// any refusal.
    Provision {
        /// Print every action as the shell line it is, changing
        /// nothing. Still requires root — the preview is the runbook's
        /// proof artifact.
        #[arg(long)]
        dry_run: bool,
        /// The built cadence-agent-exec to install — a required
        /// absolute path. The binary being installed is the setuid
        /// bridge the boundary rests on, so its source is always
        /// chosen explicitly and never discovered from the cwd, which
        /// under sudo may be agent-writable.
        #[arg(long, value_name = "PATH", required = true)]
        helper: PathBuf,
        /// The uid-1000 operator seat the boundary protects.
        #[arg(long, default_value = "ubuntu", value_name = "USER")]
        operator: String,
    },
    /// Audit every §5 artifact plus §4's negative assertions: no
    /// agent-reachable ACL under the operator's home; no uid-1000 git
    /// config (`.gitconfig`, `GIT_CONFIG_GLOBAL`, include chains)
    /// carrying `safe.directory`/`include.path` over /var/lib/cadence.
    /// Exit 0 ok, 1 warn, 2 fail.
    Doctor {
        /// Print the JSON report instead of text lines.
        #[arg(long)]
        json: bool,
        /// The uid-1000 operator seat the boundary protects.
        #[arg(long, default_value = "ubuntu", value_name = "USER")]
        operator: String,
    },
    /// Print the operator's runbook — the reviewed §5 sequence and the
    /// acceptance run, verbatim from src/agent_uid/RUNBOOK.md.
    Runbook,
}
