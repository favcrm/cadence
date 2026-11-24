use super::*;

pub(super) fn run(state_dir: PathBuf) -> Result<i32> {
    cadence_ops::mcp_agent::run(&state_dir)
}
