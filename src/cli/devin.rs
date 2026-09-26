//! CAD-535: `cadence devin` — moved verbatim from src/main.rs.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    resume: Option<String>,
    detach: bool,
    cwd: Option<PathBuf>,
    alias: Option<String>,
    role: String,
    team_role: Option<String>,
    instructions_file: Option<PathBuf>,
    worktree: Option<String>,
    bootstrap: bool,
    no_bootstrap: bool,
    auto_ready: bool,
    agents_md: bool,
    permission_mode: Option<String>,
    bypass: bool,
    cloud: bool,
    cloud_params: Option<String>,
) -> Result<i32> {
    let mut devin = DevinOpts {
        permission_mode,
        bypass,
        cloud,
        ..DevinOpts::default()
    };
    apply_cloud_params(&mut devin, &split_cloud_params(cloud_params.as_deref()))?;
    provider_launch(
        &state_dir,
        "devin",
        cwd,
        &role,
        alias,
        resume,
        instructions_file,
        detach,
        None,
        worktree.as_deref(),
        BriefMode::standalone(no_bootstrap, bootstrap),
        auto_ready,
        agents_md,
        false,
        None,
        &ClaudeOpts::default(),
        &devin,
        &CursorOpts::default(),
        &CodexOpts::default(),
        team_role.as_deref(),
        false,
    )
}
