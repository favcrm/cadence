//! CAD-535: `cadence memory` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(
    state_dir: PathBuf,
    action: cadence_agent::memory::cli::MemoryAction,
) -> Result<i32> {
    cadence_agent::memory::cli::run(&action, &state_dir)
}
