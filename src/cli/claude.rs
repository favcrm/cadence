//! CAD-535: `cadence claude` — moved verbatim from src/main.rs.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    tui: bool,
    resume: Option<String>,
    cwd: Option<PathBuf>,
    alias: Option<String>,
    role: String,
    model: Option<String>,
    provider_default_model: bool,
    team_role: Option<String>,
    effort: Option<String>,
    permission_mode: Option<String>,
    allow: Vec<String>,
    bypass: bool,
    broker_approvals: bool,
    permission_timeout_secs: Option<u64>,
    turn_idle_secs: Option<u64>,
    turn_max_secs: Option<u64>,
    instructions_file: Option<PathBuf>,
    worktree: Option<String>,
    bootstrap: bool,
    no_bootstrap: bool,
    auto_ready: bool,
    detach: bool,
    agents_md: bool,
) -> Result<i32> {
    provider_launch(
        &state_dir,
        "claude",
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
        tui,
        None,
        &ClaudeOpts {
            model,
            effort,
            permission_mode,
            allow,
            bypass,
            broker_approvals,
            permission_timeout_secs,
            turn_idle_secs,
            turn_max_secs,
            // `cadence claude` has no --confine flag — Landlock
            // confinement is the pi worker's (CAD-556).
            confine: None,
        },
        &DevinOpts::default(),
        &CursorOpts::default(),
        &CodexOpts::default(),
        team_role.as_deref(),
        provider_default_model,
    )
}
