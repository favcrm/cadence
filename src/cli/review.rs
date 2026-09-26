//! CAD-535: `cadence review` — moved verbatim from src/main.rs.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    pr: String,
    repo: Option<String>,
    full: bool,
    no_full: bool,
    no_suite_lock: bool,
    stress: u32,
    keep: bool,
    json: bool,
) -> Result<i32> {
    cadence_agent::review::run(&cadence_agent::review::Options {
        pr,
        repo,
        full: full || !no_full,
        no_suite_lock,
        stress,
        keep,
        json,
        cwd: std::env::current_dir()?,
        state_dir,
    })
}
