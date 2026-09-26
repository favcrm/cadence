//! CAD-535: `cadence milestone` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(
    state_dir: PathBuf,
    action: cadence_agent::issue::cli::MilestoneAction,
) -> Result<i32> {
    cadence_agent::issue::cli::run_milestone(&action, &state_dir)
}
