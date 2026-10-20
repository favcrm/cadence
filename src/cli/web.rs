//! CAD-1642: `cadence web` is a thin client of the daemon's platform gate.
//!
//! The `/v1/web-sites` protocol and its durable per-org intents live in
//! `cadence_agent::web_sites`. This module validates and freezes the local
//! bundle, stages it under the daemon's state directory, and calls
//! `platform_call {platform:"agenticos-web"}`. Uploads and action requests are
//! therefore audited, gated and credential-renewed by the daemon, not here.

use super::*;
use cadence_agent::remote_cli::RemoteTarget;
use cadence_agent::web_publish;
pub(crate) use cadence_agent::web_sites::WebAction;

const STAGING_DIR: &str = "web-staging";
const PLATFORM: &str = "agenticos-web";

pub(super) fn run(
    state_dir: &Path,
    action: &WebAction,
    target: &RemoteTarget,
    wake_timeout: u64,
) -> Result<i32> {
    let value = match action {
        WebAction::Prepare { input, name, spa } => {
            let site = web_publish::freeze(input)?;
            let staging = stage(state_dir, &site, name, *spa)?;
            let result = platform_call(
                state_dir,
                "prepare_site",
                target,
                json!({"org": target.org, "staging": staging, "wake_timeout": wake_timeout}),
            );
            remove_staging(state_dir, &staging);
            result?
        }
        WebAction::Publish { release } => platform_call(
            state_dir,
            "publish_site",
            target,
            json!({"org": target.org, "release": release, "wake_timeout": wake_timeout}),
        )?,
        WebAction::Rollback { slug, release } => platform_call(
            state_dir,
            "rollback_site",
            target,
            json!({
                "org": target.org,
                "slug": slug,
                "release": release,
                "wake_timeout": wake_timeout,
            }),
        )?,
        WebAction::Unpublish { slug } => platform_call(
            state_dir,
            "unpublish_site",
            target,
            json!({"org": target.org, "slug": slug, "wake_timeout": wake_timeout}),
        )?,
        WebAction::Status { slug } => platform_call(
            state_dir,
            "site_status",
            target,
            json!({"org": target.org, "slug": slug, "wake_timeout": wake_timeout}),
        )?,
        WebAction::Releases { slug } => platform_call(
            state_dir,
            "site_releases",
            target,
            json!({"org": target.org, "slug": slug, "wake_timeout": wake_timeout}),
        )?,
    };
    print_json(&value)?;
    if matches!(
        action,
        WebAction::Publish { .. } | WebAction::Rollback { .. } | WebAction::Unpublish { .. }
    ) {
        return match value["state"].as_str().unwrap_or("unknown") {
            "completed" | "awaiting_approval" | "approved" | "promoting" => Ok(0),
            state => Err(Error::rejected(format!(
                "web action ended in state '{state}'"
            ))),
        };
    }
    Ok(0)
}

fn platform_call(
    state_dir: &Path,
    tool: &str,
    target: &RemoteTarget,
    input: Value,
) -> Result<Value> {
    client::rpc(
        state_dir,
        "platform_call",
        json!({
            "platform": PLATFORM,
            "tool": tool,
            "account": target.org,
            "input": input,
        }),
    )
}

/// Freeze output is written under the daemon's state directory, one bounded
/// token per invocation. The adapter reads it; the CLI never touches the
/// network for it.
fn stage(
    state_dir: &Path,
    site: &web_publish::FrozenSite,
    slug: &str,
    spa: bool,
) -> Result<String> {
    let mut random = [0u8; 12];
    getrandom::fill(&mut random)
        .map_err(|_| Error::rejected("unable to create a staging token"))?;
    let token: String = format!(
        "stage-{}",
        random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let dir = state_dir.join(STAGING_DIR).join(&token);
    std::fs::create_dir_all(dir.join("files"))?;
    let files: Vec<Value> = site
        .files
        .iter()
        .map(|file| {
            json!({
                "path": file.path,
                "sha256": file.sha256,
                "mime": file.mime,
                "bytes": file.bytes.len(),
            })
        })
        .collect();
    let plan = json!({
        "version": "1",
        "slug": slug,
        "spa": spa,
        "digest": site.digest,
        "canonical": site.canonical,
        "files": files,
    });
    write_private(
        &dir.join("plan.json"),
        &serde_json::to_vec(&plan).map_err(|error| Error::internal(error.to_string()))?,
    )?;
    for file in &site.files {
        write_private(
            &dir.join("files")
                .join(web_publish::encode_artifact_path(&file.path)),
            &file.bytes,
        )?;
    }
    Ok(token)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn remove_staging(state_dir: &Path, token: &str) {
    let _ = std::fs::remove_dir_all(state_dir.join(STAGING_DIR).join(token));
}

fn print_json(value: &Value) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| Error::internal(error.to_string()))?
    );
    Ok(())
}
