//! CAD-535: `cadence ui` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(state_dir: PathBuf, action: cadence_agent::ui::UiAction) -> Result<i32> {
    // The board's `/api/setup` names only verbs this binary has.
    cadence_agent::setup::register_verbs(cli_verbs());
    cadence_agent::ui::run_cli(&state_dir, &action)
}
