//! CAD-1713: real AgenticOS provider profile for one dev sandbox.
//!
//! `cadence dev agenticos setup` writes a `provider.json`, a
//! sandbox-scoped `provider-deployments.json` and three `0600` credential
//! files under the sandbox root. `cadence dev up` applies the profile to
//! the daemon (and to the CLI children it starts) instead of requiring the
//! caller to export five environment variables by hand.
//!
//! Nothing here is production authority. The daemon's dev loader refuses
//! the deployment file unless the store is a dev store, and the profile is
//! only read from a path under the sandbox root.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Sandbox subdirectory holding the profile and credentials.
pub const DIR: &str = "provider";
/// The profile file under the sandbox root.
pub const PROFILE: &str = "provider.json";
/// The sandbox-scoped deployment pin consumed by the daemon.
pub const DEPLOYMENTS: &str = "provider-deployments.json";
const PROFILE_CAP: u64 = 16 * 1024;
const TOKEN_CAP: u64 = 8 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema: u32,
    pub origin: String,
    pub workspace: String,
    pub pin: String,
    pub credentials: Credentials,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    /// Combined `provider.read` + `provider.draft` credential. The host
    /// keeps one connection per provider/account, so source, image and
    /// writer share this connection when present.
    #[serde(default)]
    pub provider: Option<PathBuf>,
    pub read: PathBuf,
    pub draft: PathBuf,
    pub send: PathBuf,
}

impl Profile {
    fn validate(&self, root: &Path) -> Result<()> {
        if self.schema != 1 {
            return Err(Error::rejected("provider profile schema must be 1"));
        }
        crate::platform::agenticos_external::valid_base(&self.origin)?;
        crate::platform::connections::provider_account_identifier(
            crate::platform::agenticos_external::PLATFORM,
            &self.workspace,
        )
        .map_err(|_| {
            Error::rejected("provider workspace must be the exact ws_<uuid> account ID")
        })?;
        if !matches!(
            self.pin.as_str(),
            crate::platform::agenticos_external::MANIFEST_PIN
                | crate::platform::agenticos_external::TEXT_MANIFEST_PIN
        ) {
            return Err(Error::rejected(
                "provider manifest pin is not a reviewed agenticos-external pin",
            ));
        }
        for path in [
            self.credentials.read.as_path(),
            self.credentials.draft.as_path(),
            self.credentials.send.as_path(),
        ]
        .into_iter()
        .chain(self.credentials.provider.as_deref())
        {
            validate_credential(root, path)?;
        }
        Ok(())
    }

    /// Apply the profile to a daemon or CLI child. The caller's own
    /// `CADENCE_AGENTICOS_URL` is removed: a dev sandbox must not inherit
    /// a hosted origin from the operator's shell.
    pub fn apply_env(&self, cmd: &mut Command, root: &Path) {
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        cmd.env_remove("CADENCE_AGENTICOS_URL");
        cmd.env("CADENCE_AGENTICOS_EXTERNAL_URL", &self.origin)
            .env("CADENCE_PUBLISH_SEND_URL", &self.origin)
            .env(
                "CADENCE_PUBLISH_SEND_CREDENTIAL_FILE",
                &self.credentials.send,
            )
            .env("CADENCE_PUBLISH_READ_URL", &self.origin)
            .env(
                "CADENCE_PUBLISH_READ_CREDENTIAL_FILE",
                &self.credentials.read,
            )
            .env("CADENCE_DEV_PROVIDER_DEPLOYMENTS", deployments_path(&root));
    }

    /// The same exports for `eval "$(cadence dev env)"`.
    pub fn env_exports(&self, root: &Path) -> String {
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        format!(
            "unset CADENCE_AGENTICOS_URL\n\
             export CADENCE_AGENTICOS_EXTERNAL_URL={}\n\
             export CADENCE_PUBLISH_SEND_URL={}\n\
             export CADENCE_PUBLISH_SEND_CREDENTIAL_FILE={}\n\
             export CADENCE_PUBLISH_READ_URL={}\n\
             export CADENCE_PUBLISH_READ_CREDENTIAL_FILE={}\n\
             export CADENCE_DEV_PROVIDER_DEPLOYMENTS={}\n",
            sh_quote(&self.origin),
            sh_quote(&self.origin),
            sh_quote(&self.credentials.send.to_string_lossy()),
            sh_quote(&self.origin),
            sh_quote(&self.credentials.read.to_string_lossy()),
            sh_quote(&deployments_path(&root).to_string_lossy()),
        )
    }
}

/// Load the profile when it exists. A malformed profile refuses: silently
/// falling back to a daemon without providers would hide the breakage.
pub fn load(root: &Path) -> Result<Option<Profile>> {
    let path = root.join(PROFILE);
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::rejected(format!(
                "provider profile is unreadable: {error}"
            )))
        }
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return Err(Error::rejected(
            "provider profile must be a regular file, not a symlink",
        ));
    }
    let bytes = fs::read(&path)
        .map_err(|error| Error::rejected(format!("provider profile is unreadable: {error}")))?;
    if bytes.len() as u64 > PROFILE_CAP {
        return Err(Error::rejected("provider profile is too large"));
    }
    let profile: Profile = serde_json::from_slice(&bytes)
        .map_err(|_| Error::rejected("provider profile is malformed"))?;
    profile.validate(root)?;
    Ok(Some(profile))
}

/// Write the profile and credentials. `token_dir` must contain
/// `send.token` and either `provider.token` (read + draft) or
/// `read.token`; `draft.token` is optional and defaults to the read
/// credential.
pub fn setup(
    root: &Path,
    origin: &str,
    workspace: &str,
    pin: &str,
    token_dir: &Path,
) -> Result<Value> {
    let root = fs::canonicalize(root)
        .map_err(|_| Error::rejected("provider sandbox root is unavailable"))?;
    let dir = root.join(DIR);
    fs::create_dir_all(&dir)
        .map_err(|error| Error::rejected(format!("cannot create provider dir: {error}")))?;
    let dir_meta = fs::symlink_metadata(&dir)
        .map_err(|error| Error::rejected(format!("cannot inspect provider dir: {error}")))?;
    if !dir_meta.file_type().is_dir() {
        return Err(Error::rejected(
            "provider dir must be a real directory under the sandbox root",
        ));
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| Error::rejected(format!("cannot secure provider dir: {error}")))?;

    let provider = token_dir
        .join("provider.token")
        .is_file()
        .then(|| copy_token(token_dir, "provider.token", &dir, "provider.token"))
        .transpose()?;
    let read = if token_dir.join("read.token").is_file() {
        copy_token(token_dir, "read.token", &dir, "provider-read.token")?
    } else if let Some(provider) = provider.as_deref() {
        copy_from(provider, &dir.join("provider-read.token"))?
    } else {
        return Err(Error::rejected(
            "provider token dir needs provider.token (read + draft) or read.token",
        ));
    };
    let draft = if token_dir.join("draft.token").is_file() {
        copy_token(token_dir, "draft.token", &dir, "provider-draft.token")?
    } else {
        copy_from(&read, &dir.join("provider-draft.token"))?
    };
    let credentials = Credentials {
        provider,
        read,
        draft,
        send: copy_token(token_dir, "send.token", &dir, "publish-send.token")?,
    };
    let profile = Profile {
        schema: 1,
        origin: origin.trim_end_matches('/').to_owned(),
        workspace: workspace.to_owned(),
        pin: pin.to_owned(),
        credentials,
    };
    profile.validate(&root)?;

    let deployments = json!({
        "schema": 1,
        "providers": [{
            "provider": "agenticos_external",
            "origin": profile.origin,
            "manifest_pin": profile.pin,
        }],
    });
    let profile_file = root.join(PROFILE);
    let deployments_file = deployments_path(&root);
    write_private(
        &deployments_file,
        serde_json::to_vec_pretty(&deployments)
            .map_err(|_| Error::internal("cannot encode provider deployments"))?
            .as_slice(),
    )?;
    write_private(
        &profile_file,
        serde_json::to_vec_pretty(&profile)
            .map_err(|_| Error::internal("cannot encode provider profile"))?
            .as_slice(),
    )?;

    Ok(json!({
        "schema": 1,
        "root": root,
        "profile": profile_file,
        "deployments": deployments_file,
        "origin": profile.origin,
        "workspace": profile.workspace,
        "pin": profile.pin,
        "credentials": {
            "provider": profile.credentials.provider,
            "read": profile.credentials.read,
            "draft": profile.credentials.draft,
            "send": profile.credentials.send,
        },
    }))
}

pub fn deployments_path(root: &Path) -> PathBuf {
    root.join(DEPLOYMENTS)
}

fn copy_token(from_dir: &Path, from: &str, to_dir: &Path, to: &str) -> Result<PathBuf> {
    let source = from_dir.join(from);
    let bytes = fs::read(&source).map_err(|error| {
        Error::rejected(format!(
            "provider token {} is unreadable: {error}",
            source.display()
        ))
    })?;
    if bytes.is_empty() || bytes.len() as u64 > TOKEN_CAP {
        return Err(Error::rejected(format!(
            "provider token {} must be 1..{TOKEN_CAP} bytes",
            source.display()
        )));
    }
    let destination = to_dir.join(to);
    write_private(&destination, &bytes)?;
    Ok(destination)
}

fn copy_from(source: &Path, destination: &Path) -> Result<PathBuf> {
    let bytes = fs::read(source).map_err(|error| {
        Error::rejected(format!(
            "provider token {} is unreadable: {error}",
            source.display()
        ))
    })?;
    if bytes.is_empty() || bytes.len() as u64 > TOKEN_CAP {
        return Err(Error::rejected(format!(
            "provider token {} must be 1..{TOKEN_CAP} bytes",
            source.display()
        )));
    }
    write_private(destination, &bytes)?;
    Ok(destination.to_path_buf())
}

fn validate_credential(root: &Path, path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(Error::rejected("provider credential path must be absolute"));
    }
    let root = fs::canonicalize(root)
        .map_err(|_| Error::rejected("provider sandbox root is unavailable"))?;
    let resolved = fs::canonicalize(path)
        .map_err(|_| Error::rejected("provider credential file is unavailable"))?;
    if !resolved.starts_with(&root) {
        return Err(Error::rejected(
            "provider credential file must live under the sandbox root",
        ));
    }
    let meta = fs::symlink_metadata(path)
        .map_err(|_| Error::rejected("provider credential file is unavailable"))?;
    if !meta.file_type().is_file() {
        return Err(Error::rejected(
            "provider credential must be a regular file",
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(Error::rejected(
            "provider credential file must be mode 0600",
        ));
    }
    if meta.len() == 0 || meta.len() > TOKEN_CAP {
        return Err(Error::rejected("provider credential has an invalid size"));
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() || !meta.file_type().is_file() || meta.nlink() > 1 {
            return Err(Error::rejected(format!(
                "refusing to write through {}",
                path.display()
            )));
        }
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| Error::rejected(format!("cannot write {}: {error}", path.display())))?;
    file.write_all(bytes)
        .map_err(|error| Error::rejected(format!("cannot write {}: {error}", path.display())))?;
    file.sync_all()
        .map_err(|error| Error::rejected(format!("cannot sync {}: {error}", path.display())))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| Error::rejected(format!("cannot secure {}: {error}", path.display())))?;
    Ok(())
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(dir: &Path, name: &str) {
        fs::write(dir.join(name), format!("token-{name}")).unwrap();
    }

    #[test]
    fn setup_writes_a_loadable_private_profile() {
        let root = tempfile::tempdir().unwrap();
        let tokens = tempfile::tempdir().unwrap();
        token(tokens.path(), "read.token");
        token(tokens.path(), "draft.token");
        token(tokens.path(), "send.token");
        let out = setup(
            root.path(),
            "https://api-v2.agenticos.hk",
            "ws_11111111-1111-1111-1111-111111111111",
            crate::platform::agenticos_external::TEXT_MANIFEST_PIN,
            tokens.path(),
        )
        .unwrap();
        let profile = load(root.path()).unwrap().expect("profile");
        assert_eq!(profile.origin, "https://api-v2.agenticos.hk");
        assert_eq!(
            profile.credentials.read,
            out["credentials"]["read"]
                .as_str()
                .map(PathBuf::from)
                .unwrap()
        );
        for path in [
            &profile.credentials.read,
            &profile.credentials.draft,
            &profile.credentials.send,
        ] {
            assert_eq!(
                fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let deployments = crate::platform::deployments::DeploymentMetadata::parse(
            &fs::read(deployments_path(root.path())).unwrap(),
        )
        .unwrap();
        assert_eq!(
            deployments.pin("agenticos_external", "https://api-v2.agenticos.hk"),
            Some(profile.pin.as_str())
        );
    }

    #[test]
    fn setup_accepts_one_combined_provider_token() {
        let root = tempfile::tempdir().unwrap();
        let tokens = tempfile::tempdir().unwrap();
        token(tokens.path(), "provider.token");
        token(tokens.path(), "send.token");
        setup(
            root.path(),
            "https://api-v2.agenticos.hk",
            "ws_11111111-1111-1111-1111-111111111111",
            crate::platform::agenticos_external::TEXT_MANIFEST_PIN,
            tokens.path(),
        )
        .unwrap();
        let profile = load(root.path()).unwrap().expect("profile");
        assert!(profile.credentials.provider.is_some());
        assert_eq!(
            fs::read(&profile.credentials.read).unwrap(),
            b"token-provider.token"
        );
        assert_eq!(
            fs::read(&profile.credentials.draft).unwrap(),
            b"token-provider.token"
        );
    }

    #[test]
    fn setup_refuses_a_symlinked_output() {
        let root = tempfile::tempdir().unwrap();
        let tokens = tempfile::tempdir().unwrap();
        token(tokens.path(), "provider.token");
        token(tokens.path(), "send.token");
        let target = tempfile::NamedTempFile::new().unwrap();
        fs::write(target.path(), b"do not touch").unwrap();
        let dir = root.path().join(DIR);
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(target.path(), dir.join("provider.token")).unwrap();
        let error = setup(
            root.path(),
            "https://api-v2.agenticos.hk",
            "ws_11111111-1111-1111-1111-111111111111",
            crate::platform::agenticos_external::TEXT_MANIFEST_PIN,
            tokens.path(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("refusing"), "{error}");
        assert_eq!(fs::read(target.path()).unwrap(), b"do not touch");
    }

    #[test]
    fn profile_refuses_a_credential_outside_the_sandbox() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let mut profile = Profile {
            schema: 1,
            origin: "https://api-v2.agenticos.hk".into(),
            workspace: "ws_11111111-1111-1111-1111-111111111111".into(),
            pin: crate::platform::agenticos_external::TEXT_MANIFEST_PIN.into(),
            credentials: Credentials {
                provider: None,
                read: outside.path().to_path_buf(),
                draft: outside.path().to_path_buf(),
                send: outside.path().to_path_buf(),
            },
        };
        assert!(profile.validate(root.path()).is_err());
        profile.credentials.read = root.path().join("read.token");
        assert!(profile.validate(root.path()).is_err());
    }
}
