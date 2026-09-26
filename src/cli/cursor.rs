//! CAD-535: `cadence cursor` — moved verbatim from src/main.rs.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    resume: Option<String>,
    detach: bool,
    cwd: Option<PathBuf>,
    alias: Option<String>,
    role: String,
    model: Option<String>,
    provider_default_model: bool,
    team_role: Option<String>,
    permission_mode: Option<String>,
    bypass: bool,
    instructions_file: Option<PathBuf>,
    worktree: Option<String>,
    bootstrap: bool,
    no_bootstrap: bool,
    auto_ready: bool,
    agents_md: bool,
) -> Result<i32> {
    provider_launch(
        &state_dir,
        "cursor",
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
        &DevinOpts::default(),
        &CursorOpts {
            model,
            permission_mode,
            bypass,
        },
        &CodexOpts::default(),
        team_role.as_deref(),
        provider_default_model,
    )
}
