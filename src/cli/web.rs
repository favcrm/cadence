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
    // The daemon renews, but the command still refuses locally when no saved
    // credential exists — the pre-cutover exit code and message are kept.
    cadence_agent::remote_cli::web_credential_saved(target)?;
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
        WebAction::Submissions {
            slug,
            form,
            limit,
            cursor,
        } => platform_call(
            state_dir,
            "site_submissions",
            target,
            json!({
                "org": target.org,
                "slug": slug,
                "form": form,
                "limit": limit,
                "cursor": cursor,
                "wake_timeout": wake_timeout,
            }),
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
    let response = client::rpc(
        state_dir,
        "platform_call",
        json!({
            "platform": PLATFORM,
            "tool": tool,
            "account": target.org,
            "input": input,
        }),
    )?;
    // `platform_call` answers {result, effect, platform_result}: the command
    // prints the adapter's own result, as it did before the cutover.
    Ok(match response {
        Value::Object(mut envelope) => match envelope.remove("platform_result") {
            Some(result) => result,
            None => Value::Object(envelope),
        },
        other => other,
    })
}

/// Validate the pathname ancestors: the daemon reopens the staging root, so
/// another UID must not be able to replace any component after staging.
fn trusted_state_dir(path: &Path) -> Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Component;

    let relative = if path.is_relative() {
        Some(std::env::current_dir()?.join(path))
    } else {
        None
    };
    let absolute = relative.as_deref().unwrap_or(path);
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let mut dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open("/")?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let unsafe_path = || {
        Error::rejected(format!(
            "web staging requires an owner-controlled state directory, ancestors not writable \
             by other users, and no symlinks: {}; secure the path before preparing a site",
            path.display()
        ))
    };
    let mut meta = dir.metadata()?;
    for part in absolute.components() {
        let name = match part {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            Component::ParentDir => std::ffi::OsStr::new(".."),
            Component::Prefix(_) => return Err(unsafe_path()),
        };
        if (meta.uid() != uid && meta.uid() != 0)
            || (meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0)
        {
            return Err(unsafe_path());
        }
        let name = CString::new(name.as_bytes()).map_err(|_| unsafe_path())?;
        // SAFETY: dir is live, name is NUL-terminated, and openat returns a new fd.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(unsafe_path());
        }
        // SAFETY: a successful openat returned a uniquely owned descriptor.
        dir = unsafe { std::fs::File::from_raw_fd(fd) };
        meta = dir.metadata()?;
        if meta.uid() != uid && meta.uid() != 0 {
            return Err(unsafe_path());
        }
    }
    if meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return Err(unsafe_path());
    }
    Ok(dir)
}

/// Create or securely tighten the application-owned staging root.
fn staging_root(state_dir: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

    let root = state_dir.join(STAGING_DIR);
    match std::fs::DirBuilder::new().mode(0o700).create(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&root)?;
    let meta = dir.metadata()?;
    // SAFETY: geteuid has no preconditions.
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(Error::rejected(
            "web staging root is not owned by the current user",
        ));
    }
    if meta.mode() & 0o7777 != 0o700 {
        // File::set_permissions acts on the pinned fd, not on a replaceable path.
        dir.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(root)
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
    use std::os::unix::fs::DirBuilderExt;

    let _state = trusted_state_dir(state_dir)?;
    let root = staging_root(state_dir)?;
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
    let dir = root.join(&token);
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    if let Err(error) = std::fs::DirBuilder::new()
        .mode(0o700)
        .create(dir.join("files"))
    {
        let _ = std::fs::remove_dir(&dir);
        return Err(error.into());
    }
    if let Err(error) = stage_plan(&dir, site, slug, spa) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(error);
    }
    Ok(token)
}

fn stage_plan(dir: &Path, site: &web_publish::FrozenSite, slug: &str, spa: bool) -> Result<()> {
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
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
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
        cadence_agent::output::json_text(value)
            .map_err(|error| Error::internal(error.to_string()))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture() -> (tempfile::TempDir, PathBuf, web_publish::FrozenSite) {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.path().join("state");
        let source = root.path().join("source");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("index.html"), "synthetic unpublished content").unwrap();
        let site = web_publish::freeze(&source).unwrap();
        (root, state, site)
    }

    #[test]
    fn web_staging_is_private_under_permissive_umask() {
        const CHILD: &str = "CADENCE_TEST_WEB_STAGE_UMASK_CHILD";
        const TEST: &str = "cli::web::tests::web_staging_is_private_under_permissive_umask";
        if std::env::var_os(CHILD).is_some() {
            let (root, state, site) = fixture();
            let control = root.path().join("control");
            std::fs::create_dir(&control).unwrap();
            assert_eq!(
                std::fs::metadata(control).unwrap().permissions().mode() & 0o777,
                0o775
            );
            let token = stage(&state, &site, "synthetic", false).unwrap();
            let staging = state.join(STAGING_DIR);
            let dir = staging.join(&token);
            for path in [&staging, &dir, &dir.join("files")] {
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
            let file = dir
                .join("files")
                .join(web_publish::encode_artifact_path("index.html"));
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                "synthetic unpublished content"
            );
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            remove_staging(&state, &token);
            assert!(!dir.exists());

            // Existing owner-controlled roots from older versions are tightened safely.
            std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o775)).unwrap();
            let token = stage(&state, &site, "synthetic", false).unwrap();
            assert_eq!(
                std::fs::metadata(&staging).unwrap().permissions().mode() & 0o777,
                0o700
            );
            remove_staging(&state, &token);
            return;
        }
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("umask 0002 && exec \"$0\" --exact \"$1\" --test-threads=1")
            .arg(std::env::current_exe().unwrap())
            .arg(TEST)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TMPDIR", std::env::temp_dir())
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn web_staging_refuses_unsafe_ancestors_and_existing_files() {
        let (root, state, site) = fixture();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(stage(&state, &site, "synthetic", false).is_err());
        assert!(!state.join(STAGING_DIR).exists());

        let ancestor = root.path().join("shared");
        let nested = ancestor.join("state");
        std::fs::create_dir(&ancestor).unwrap();
        std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o775)).unwrap();
        std::fs::create_dir(&nested).unwrap();
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(stage(&nested, &site, "synthetic", false).is_err());
        assert!(!nested.join(STAGING_DIR).exists());

        let safe = root.path().join("safe");
        let foreign = root.path().join("foreign");
        std::fs::create_dir(&safe).unwrap();
        std::fs::set_permissions(&safe, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("sentinel"), "untouched").unwrap();
        std::os::unix::fs::symlink(&foreign, safe.join(STAGING_DIR)).unwrap();
        assert!(stage(&safe, &site, "synthetic", false).is_err());
        assert_eq!(
            std::fs::read_to_string(foreign.join("sentinel")).unwrap(),
            "untouched"
        );

        let planted = root.path().join("planted");
        std::fs::write(&planted, "untouched").unwrap();
        assert!(write_private(&planted, b"replacement").is_err());
        assert_eq!(std::fs::read_to_string(planted).unwrap(), "untouched");
    }
}
