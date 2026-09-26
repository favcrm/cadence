//! CAD-535: `cadence intake` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum IntakeAction {
    /// Write or update one project's relay configuration.  The relay is
    /// disabled unless --enable is explicitly supplied.
    Configure {
        /// PM project key whose local intake issues are published.
        project: String,
        /// GitHub owner/name. URLs, tokens and credential-shaped values are
        /// rejected.
        repo: String,
        /// Explicitly enable this project.
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        /// Explicitly disable this project while retaining its state.
        #[arg(long, conflicts_with = "enable")]
        disable: bool,
        /// Minimum seconds between non-model polls.
        #[arg(long, default_value_t = cadence_agent::issue::relay::DEFAULT_POLL_SECONDS,
              value_parser = clap::value_parser!(u64).range(1..))]
        poll_seconds: u64,
        /// Permit on-demand PM dispatch after quota/agent checks.  A sync
        /// still needs its separate --dispatch flag.
        #[arg(long)]
        dispatch: bool,
        /// PM alias to receive actionable comments.
        #[arg(long)]
        pm: Option<String>,
        /// GitHub login used for self-echo suppression.
        #[arg(long)]
        actor: Option<String>,
    },
    /// Show config, heartbeat, delivery receipts, cursors and durable action
    /// states. This never contacts GitHub or a provider.
    Status {
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Run one poll with --once, or keep a cheap non-model polling loop.
    Sync {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        once: bool,
        /// Permit configured, quota-checked actionable PM dispatches.
        #[arg(long)]
        dispatch: bool,
    },
    /// Make one receipt eligible for the next sync attempt.
    Retry { project: String, report: String },
}

pub(super) fn run(state_dir: PathBuf, action: IntakeAction) -> Result<i32> {
    match action {
        IntakeAction::Configure {
            project,
            repo,
            enable,
            disable,
            poll_seconds,
            dispatch,
            pm,
            actor,
        } => {
            if enable == disable {
                return Err(Error::rejected(
                    "intake configure requires exactly one of --enable or --disable",
                ));
            }
            let result = cadence_agent::issue::relay::configure(
                &state_dir,
                &project,
                &repo,
                enable,
                poll_seconds,
                dispatch,
                pm,
                actor,
            )?;
            print_json(&result);
            Ok(0)
        }
        IntakeAction::Status { project, .. } => {
            print_json(&cadence_agent::issue::relay::status(
                &state_dir,
                project.as_deref(),
            )?);
            Ok(0)
        }
        IntakeAction::Sync {
            project,
            once,
            dispatch,
        } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            let result = cadence_agent::issue::relay::run_loop(
                &pm.dir,
                &state_dir,
                project.as_deref(),
                dispatch,
                once,
            )?;
            print_json(&result);
            Ok(0)
        }
        IntakeAction::Retry { project, report } => {
            print_json(&cadence_agent::issue::relay::retry(
                &state_dir, &project, &report,
            )?);
            Ok(0)
        }
    }
}
