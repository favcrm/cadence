//! CAD-535: `cadence attach` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(state_dir: PathBuf, name: Option<String>, print: bool) -> Result<i32> {
    attach_command(&state_dir, name, print)
}
