//! CAD-535: `cadence join` — moved verbatim from src/main.rs.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    group: String,
    provider: String,
    resume: Option<String>,
    tui: bool,
    detach: bool,
    cwd: Option<PathBuf>,
    alias: Option<String>,
    role: String,
    sandbox: Option<String>,
    instructions_file: Option<PathBuf>,
    worktree: Option<String>,
    no_bootstrap: bool,
    auto_ready: bool,
    agents_md: bool,
    model: Option<String>,
    provider_default_model: bool,
    team_role: Option<String>,
    effort: Option<String>,
    approval_policy: Option<String>,
    permission_mode: Option<String>,
    allow: Vec<String>,
    bypass: bool,
    turn_idle_secs: Option<u64>,
    turn_max_secs: Option<u64>,
    broker_approvals: bool,
    permission_timeout_secs: Option<u64>,
    cloud: bool,
    cloud_params: Option<String>,
    confine: bool,
    no_confine: bool,
) -> Result<i32> {
    let mut devin = DevinOpts {
        permission_mode: permission_mode.clone(),
        bypass,
        cloud,
        ..DevinOpts::default()
    };
    apply_cloud_params(&mut devin, &split_cloud_params(cloud_params.as_deref()))?;
    join_group(
        &state_dir,
        &group,
        &provider,
        resume,
        tui,
        detach,
        cwd,
        alias,
        &role,
        sandbox,
        instructions_file,
        worktree,
        no_bootstrap,
        auto_ready,
        agents_md,
        ClaudeOpts {
            model: model.clone(),
            effort: effort.clone(),
            permission_mode: permission_mode.clone(),
            allow,
            bypass,
            broker_approvals,
            permission_timeout_secs,
            turn_idle_secs,
            turn_max_secs,
            confine: if confine {
                Some(true)
            } else {
                no_confine.then_some(false)
            },
        },
        // The shared --permission-mode/--bypass flags feed the
        // devin worker too — its four-mode vocabulary is validated
        // in provider_launch.
        devin,
        // …and the cursor worker — its two-mode vocabulary is
        // validated in provider_launch the same way.
        CursorOpts {
            model: model.clone(),
            permission_mode,
            bypass,
        },
        CodexOpts {
            model,
            effort,
            approval_policy,
            turn_idle_secs,
            turn_max_secs,
        },
        team_role,
        provider_default_model,
    )
}
