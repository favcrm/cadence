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

pub(crate) fn split_cloud_params(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn apply_cloud_params(opts: &mut DevinOpts, raw: &[String]) -> Result<()> {
    for entry in raw {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| Error::rejected("--cloud-params entries must be key=value"))?;
        if value.is_empty() {
            return Err(Error::rejected(format!(
                "--cloud-params '{key}' needs a value"
            )));
        }
        match key {
            "repo" => opts.repos.push(value.to_string()),
            "devin_mode" => {
                if !registry::DEVIN_CLOUD_MODES.contains(&value) {
                    return Err(Error::rejected(format!(
                        "unknown devin_mode '{value}' — expected one of: {}",
                        registry::DEVIN_CLOUD_MODES.join(", ")
                    )));
                }
                opts.devin_mode = Some(value.to_string());
            }
            "max_acu_limit" => {
                let limit: u64 = value
                    .parse()
                    .map_err(|_| Error::rejected("'max_acu_limit' must be a positive integer"))?;
                if limit == 0 {
                    return Err(Error::rejected(
                        "'max_acu_limit' must be a positive integer",
                    ));
                }
                opts.max_acu_limit = Some(limit);
            }
            "playbook_id" => opts.playbook_id = Some(value.to_string()),
            "knowledge_id" => opts.knowledge_ids.push(value.to_string()),
            "secret_id" => opts.secret_ids.push(value.to_string()),
            "platform" => opts.platform = Some(value.to_string()),
            "tag" => opts.tags.push(value.to_string()),
            "bypass_approval" => {
                opts.bypass_approval = match value {
                    "true" => true,
                    "false" => false,
                    _ => return Err(Error::rejected("bypass_approval must be true or false")),
                };
            }
            "attachment_url" => opts.attachment_urls.push(value.to_string()),
            other => {
                return Err(Error::rejected(format!(
                    "unknown --cloud-params key '{other}' — expected repo, devin_mode, \
                     max_acu_limit, playbook_id, knowledge_id, secret_id, platform, \
                     tag, bypass_approval, or attachment_url"
                )))
            }
        }
    }
    Ok(())
}

pub(crate) fn insert_devin_cloud_params(
    params: &mut serde_json::Map<String, Value>,
    devin: &DevinOpts,
) -> Result<()> {
    if devin.permission_mode.is_some() || devin.bypass {
        return Err(Error::rejected(
            "--permission-mode and --bypass do not apply to a Devin cloud session",
        ));
    }
    if !devin.repos.is_empty() {
        params.insert("repos".to_string(), json!(devin.repos));
    }
    if let Some(mode) = &devin.devin_mode {
        params.insert("devin_mode".to_string(), json!(mode));
    }
    if let Some(limit) = devin.max_acu_limit {
        params.insert("max_acu_limit".to_string(), json!(limit));
    }
    if let Some(id) = &devin.playbook_id {
        params.insert("playbook_id".to_string(), json!(id));
    }
    if !devin.knowledge_ids.is_empty() {
        params.insert("knowledge_ids".to_string(), json!(devin.knowledge_ids));
    }
    if !devin.secret_ids.is_empty() {
        params.insert("secret_ids".to_string(), json!(devin.secret_ids));
    }
    if let Some(platform) = &devin.platform {
        params.insert("platform".to_string(), json!(platform));
    }
    if !devin.tags.is_empty() {
        params.insert("tags".to_string(), json!(devin.tags));
    }
    if devin.bypass_approval {
        params.insert("bypass_approval".to_string(), json!(true));
    }
    if !devin.attachment_urls.is_empty() {
        params.insert("attachment_urls".to_string(), json!(devin.attachment_urls));
    }
    Ok(())
}
