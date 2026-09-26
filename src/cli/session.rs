//! CAD-535: `cadence session` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum SessionAction {
    /// Start-of-session gate: host, binary-vs-main, daemon, board,
    /// reconcile, inbox — one screen, exit 0 ok / 1 warnings /
    /// 2 failures. Judges the cwd repo's project by default; other
    /// projects collapse to one summary line that never fails the
    /// gate. Read-only by default.
    Start {
        /// Judge this project instead of the cwd repo's.
        #[arg(long, conflicts_with = "all")]
        project: Option<String>,
        /// Judge every project — the fleet-wide gate.
        #[arg(long)]
        all: bool,
        /// Emit the check report as JSON.
        #[arg(long)]
        json: bool,
        /// Perform only the reversible fixes: `daemon start`, `ui
        /// start`, `ui tailscale start` when sharing is persisted.
        /// Never restarts a running daemon, never removes anything.
        #[arg(long)]
        fix: bool,
        /// Read the host report from this JSON file instead of
        /// scanning — tests/debug; the run is labelled fixture-backed.
        #[arg(long, hide = true)]
        host_report: Option<PathBuf>,
    },
    /// End-of-session: `issue finish --merged` when this build has it,
    /// stop agents idle past --idle-secs, `agent gc --older-than 1h`,
    /// the host sweep (orphan test processes are reported, never
    /// killed), and the handoff note under <state>/sessions/.
    /// Never stops a busy agent; never stops the daemon.
    End {
        /// Scope tracker reads and repo scans to one project key.
        #[arg(long)]
        project: Option<String>,
        /// Emit the step report as JSON.
        #[arg(long)]
        json: bool,
        /// Print the plan — candidates listed, nothing changed.
        #[arg(long)]
        dry_run: bool,
        /// Accepted for muscle memory — the merged sweep has no force
        /// path; recorded as ignored in the run's notes.
        #[arg(long)]
        force_finish: bool,
        /// An agent idle longer than this (seconds) may be stopped
        /// [default 1800].
        #[arg(long, default_value_t = 1800)]
        idle_secs: u64,
        /// Read the host report from this JSON file instead of
        /// scanning — tests/debug; the run is labelled fixture-backed.
        #[arg(long, hide = true)]
        host_report: Option<PathBuf>,
    },
    /// Acknowledge a known `session start` item by the key it prints
    /// in [brackets]: until the ack expires the item warns instead of
    /// failing, and still prints with the reason. `--list` shows every
    /// ack, expired ones included.
    Ack {
        /// The item key, e.g. `reconcile:<message-id>`, `host:disk`.
        #[arg(required_unless_present = "list")]
        key: Option<String>,
        /// Why the item is known and parked.
        #[arg(long, required_unless_present = "list")]
        reason: Option<String>,
        /// A duration (90m, 12h, 3d) or YYYY-MM-DDTHH:MM:SSZ — at most
        /// 14 days out.
        #[arg(long, required_unless_present = "list")]
        expires: Option<String>,
        /// List every acknowledgement, expired ones marked expired.
        #[arg(long, conflicts_with_all = ["key", "reason", "expires"])]
        list: bool,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
}

pub(super) fn run(state_dir: PathBuf, action: SessionAction) -> Result<i32> {
    match action {
        SessionAction::Start {
            project,
            all,
            json,
            fix,
            host_report,
        } => cadence_agent::session::run_start(&cadence_agent::session::StartOptions {
            project,
            all,
            json,
            fix,
            host_report,
            cwd: std::env::current_dir()?,
            state_dir,
        }),
        SessionAction::End {
            project,
            json,
            dry_run,
            force_finish,
            idle_secs,
            host_report,
        } => cadence_agent::session::run_end(&cadence_agent::session::EndOptions {
            project,
            json,
            dry_run,
            force_finish,
            idle_secs,
            host_report,
            cwd: std::env::current_dir()?,
            state_dir,
        }),
        SessionAction::Ack {
            key,
            reason,
            expires,
            list,
            json,
        } => cadence_agent::session::run_ack(&cadence_agent::session::AckOptions {
            key,
            reason,
            expires,
            list,
            json,
            state_dir,
        }),
    }
}
