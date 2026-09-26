//! CAD-535: `cadence codex` — moved verbatim from src/main.rs.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    detach: bool,
    cwd: Option<PathBuf>,
    alias: Option<String>,
    role: String,
    model: Option<String>,
    provider_default_model: bool,
    team_role: Option<String>,
    effort: Option<String>,
    approval_policy: Option<String>,
    turn_idle_secs: Option<u64>,
    turn_max_secs: Option<u64>,
    sandbox: Option<String>,
    instructions_file: Option<PathBuf>,
    worktree: Option<String>,
    bootstrap: bool,
    no_bootstrap: bool,
    tui: bool,
    agents_md: bool,
) -> Result<i32> {
    provider_launch(
        &state_dir,
        "codex",
        cwd,
        &role,
        alias,
        None,
        instructions_file,
        detach,
        None,
        worktree.as_deref(),
        BriefMode::standalone(no_bootstrap, bootstrap),
        false,
        agents_md,
        tui,
        sandbox,
        &ClaudeOpts::default(),
        &DevinOpts::default(),
        &CursorOpts::default(),
        &CodexOpts {
            model,
            effort,
            approval_policy,
            turn_idle_secs,
            turn_max_secs,
        },
        team_role.as_deref(),
        provider_default_model,
    )
}
