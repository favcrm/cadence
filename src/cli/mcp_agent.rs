use super::*;

pub(super) fn run(state_dir: PathBuf) -> Result<i32> {
    cadence_agent::mcp_agent::run(&state_dir)
}
