//! CAD-535: `cadence confine` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(
    _state_dir: PathBuf,
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
    command: Vec<String>,
) -> Result<i32> {
    cadence_agent::confine::exec(&cadence_agent::confine::Policy { read, write }, &command)
}
