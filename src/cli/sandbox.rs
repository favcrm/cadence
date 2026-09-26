//! CAD-535: `cadence sandbox` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(
    _state_dir: PathBuf,
    action: cadence_agent::sandbox::SandboxAction,
) -> Result<i32> {
    cadence_agent::sandbox::run_cli(&action)
}
