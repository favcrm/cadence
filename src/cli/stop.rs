//! CAD-535: `cadence stop` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(state_dir: PathBuf, group: String) -> Result<i32> {
    stop_group(&state_dir, &group)
}
