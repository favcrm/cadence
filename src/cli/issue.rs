//! CAD-535: `cadence issue` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(
    state_dir: PathBuf,
    action: cadence_agent::issue::cli::IssueAction,
) -> Result<i32> {
    cadence_agent::issue::cli::run(&action, &state_dir)
}
