//! CAD-535: `cadence backup` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(
    state_dir: PathBuf,
    dir: Option<PathBuf>,
    keep: u64,
    reason: String,
) -> Result<i32> {
    let dir = dir.unwrap_or_else(|| cadence_agent::backup::default_dir(&state_dir));
    print_json(&cadence_agent::backup::backup(
        &state_dir,
        &dir,
        keep as usize,
        &reason,
    )?);
    Ok(0)
}
