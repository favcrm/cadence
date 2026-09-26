//! CAD-535: `cadence resume` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(
    state_dir: PathBuf,
    group: Option<String>,
    all: bool,
    detach: bool,
) -> Result<i32> {
    resume_command(&state_dir, group, all, detach)
}
