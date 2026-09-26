//! CAD-535: `cadence daemon` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum DaemonAction {
    /// Run the daemon in the foreground.
    Run {
        /// Holder forwarded by `daemon start` / `daemon restart`.
        /// Not an operator flag — the public switch is `daemon start --as`.
        #[arg(long, hide = true)]
        rollout_as: Option<String>,
    },
    /// Start the daemon detached and print its state.
    Start {
        /// After the daemon is up, resume every resumable registered
        /// agent with no live endpoint.
        #[arg(long)]
        resume: bool,
        /// Identity when this shell is not a cadence pane. Required to
        /// start a binary whose commit differs from the one the daemon
        /// last recorded, and only if that identity holds the lease.
        #[arg(long = "as")]
        as_identity: Option<String>,
    },
    /// Report daemon health, including `agent_gc_timer`: whether the
    /// opt-in agent-gc timer is on (pm.yaml `[host]
    /// agent_gc_older_than_secs`), its effective age, and its last sweep;
    /// and `agent_auto_stop`: the idle auto-stop bound (default ON, 3600s;
    /// `[host] auto_stop_idle_secs`, `auto_stop_idle_secs_by_provider`),
    /// what it last stopped, and why each live agent was kept.
    Status,
    /// Ask the daemon to shut down gracefully, then wait until the
    /// process has actually exited and released the state-dir lock
    /// (bounded, 30s) — `stop && start` no longer races the drain.
    /// `daemon stop` is lease-free. A following start of the same
    /// build is also lease-free.
    Stop,
    /// Stop, wait for exit, start, and report a before/after table of
    /// every agent's state (and pane pid for pty agents). The lease
    /// gates a build change and a schema crossing. This command still
    /// asks for the lease, but a same-build `daemon stop` followed by
    /// `daemon start` (or a crash restart of the same build) stays
    /// lease-free, so that same-build requirement is advisory.
    Restart {
        /// First wait until every pty pane probes idle and no managed
        /// agent has a running message; on timeout nothing is changed.
        #[arg(long)]
        when_idle: bool,
        /// Seconds --when-idle waits for a quiet fleet before giving
        /// up [default: 1800].
        #[arg(long, default_value_t = 1800, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
        /// With --when-idle, carry the restart past stale turns —
        /// `running`/`submitted` messages on stopped or dead agents,
        /// which can never report. Default refuses, naming each stale
        /// turn and its remedy (CAD-503).
        #[arg(long)]
        ignore_stale: bool,
        /// Also restart the detached `cadence ui` server when one is
        /// running for this state dir.
        #[arg(long)]
        ui: bool,
        /// Identity when this shell is not a cadence pane. The restart
        /// is refused unless this identity holds the rollout lease.
        #[arg(long = "as")]
        as_identity: Option<String>,
    },
}

pub(super) fn run(state_dir: PathBuf, action: DaemonAction) -> Result<i32> {
    match action {
        DaemonAction::Run { rollout_as } => {
            // CAD-308: before anything is spawned, so every tree the
            // daemon launches keeps its orphans under the daemon.
            cadence_agent::reaper::enable()?;
            cadence_agent::rollout::set_forwarded_identity(rollout_as);
            // Never keep the holder in the environment, even if the
            // parent shell exported it. Panes inherit the daemon's env.
            std::env::remove_var("CADENCE_ROLLOUT_AS");
            std::fs::create_dir_all(&state_dir)?;
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
            }
            // Every daemon start re-syncs the vendored skill: a
            // rebuilt binary propagates changes. stderr lands in
            // daemon.log for the detached child — stdout stays silent.
            // A sandbox shares $HOME with production, so it never
            // writes the skill there.
            if cadence_agent::sandbox::profile().is_some() {
                eprintln!("skill: skipped (sandbox profile)");
            } else {
                match home_dir().map(|h| cadence_agent::skill::sync(&h, false)) {
                    Ok(Ok(report)) if report["wrote"].as_bool().unwrap_or(false) => {
                        eprintln!("skill: refreshed {}", report["installed"])
                    }
                    Ok(Err(e)) => eprintln!("skill: refresh failed: {e}"),
                    _ => {}
                }
            }
            cadence_agent::daemon::serve(&state_dir)?;
            Ok(0)
        }
        DaemonAction::Start {
            resume,
            as_identity,
        } => {
            std::fs::create_dir_all(&state_dir)?;
            // CAD-396/407: an interrupted `restore --force` leaves the
            // old store renamed aside; a daemon started now would create
            // an empty store next to it. `serve` refuses too; this says
            // so before anything is spawned.
            cadence_agent::backup::refuse_interrupted_restore(&state_dir)?;
            let mut result = client::daemon_start_as(&state_dir, as_identity.as_deref())?;
            // --resume: once the daemon answers, sweep every agent
            // with a stored thread and no live endpoint.
            if resume {
                let targets = client::rpc(&state_dir, "agent_list", json!({}))?["agents"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter(|a| {
                        a["endpoint"].is_null()
                            && (a["thread_id"].is_string() || a["session_id"].is_string())
                    })
                    .filter_map(|a| a["alias"].as_str().map(str::to_string))
                    .collect::<Vec<_>>();
                result["resume"] = resume_sweep(&state_dir, &targets);
            }
            print_json(&result);
            Ok(0)
        }
        DaemonAction::Status => {
            let health = client::rpc(&state_dir, "health", json!({}))?;
            print_json(&health);
            Ok(0)
        }
        DaemonAction::Stop => daemon_stop(&state_dir),
        DaemonAction::Restart {
            when_idle,
            timeout,
            ignore_stale,
            ui,
            as_identity,
        } => daemon_restart(
            &state_dir,
            when_idle,
            timeout,
            ignore_stale,
            ui,
            as_identity,
        ),
    }
}
