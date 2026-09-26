//! CAD-535: `cadence setup` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(state_dir: PathBuf, json: bool, port: Option<u16>, _: bool) -> Result<i32> {
    cadence_agent::setup::cli(&state_dir, port, json, cli_verbs())
}
