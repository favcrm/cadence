//! CAD-535: `cadence restore` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(
    state_dir: PathBuf,
    source: PathBuf,
    repos: Vec<PathBuf>,
    force: bool,
) -> Result<i32> {
    let opts = cadence_agent::backup::RestoreOptions { force, repos };
    print_json(&cadence_agent::backup::restore(&source, &state_dir, &opts)?);
    Ok(0)
}
