//! CAD-535: `cadence export` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(state_dir: PathBuf, out: PathBuf) -> Result<i32> {
    print_json(&cadence_agent::backup::export(&state_dir, &out)?);
    Ok(0)
}
