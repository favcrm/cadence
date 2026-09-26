//! CAD-535: `cadence mcp-permission` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(_state_dir: PathBuf, timeout_secs: Option<u64>) -> Result<i32> {
    cadence_agent::mcp::run(timeout_secs)
}
