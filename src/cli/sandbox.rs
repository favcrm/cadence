//! CAD-535: `cadence sandbox` — moved verbatim from src/main.rs.
//! CAD-1187: now `cadence dev` (`sandbox` stays an alias).

use super::*;

pub(super) fn run(
    state_dir: PathBuf,
    action: cadence_agent::sandbox::SandboxAction,
) -> Result<i32> {
    cadence_agent::sandbox::run_cli(&state_dir, &action)
}
