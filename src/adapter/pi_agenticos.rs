//! A credentialless, reviewed catalog for hosted AgenticOS workers.
use crate::{Error, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const CATALOG: &str = include_str!("pi_agenticos_models.json");
const MODEL: &str = "agenticos/z-ai/glm-5.3-flash";

/// Selecting this provider is still subject to the operator's model allowlist.
/// Require the reviewed catalog in the daemon's own config, then write our
/// literal credentialless projection, never arbitrary provider keys or auth.
pub(super) fn seed(dir: &Path, master: &Path, model: &str) -> Result<()> {
    if model != MODEL {
        return Err(Error::rejected("unsupported AgenticOS worker model"));
    }
    require_empty_auth(dir)?;
    let source = master.join("models.json");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source)
        .map_err(|_| Error::rejected("AgenticOS worker catalog is unavailable"))?;
    if !file.metadata()?.is_file() {
        return Err(Error::rejected(
            "AgenticOS worker catalog must be a regular file",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(65_537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        return Err(Error::rejected(
            "AgenticOS worker catalog exceeds its limit",
        ));
    }
    let catalog: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::rejected("AgenticOS worker catalog is invalid"))?;
    let reviewed: Value = serde_json::from_str(CATALOG).expect("committed AgenticOS catalog");
    if catalog["providers"]["agenticos"] != reviewed["providers"]["agenticos"] {
        return Err(Error::rejected(
            "AgenticOS worker catalog differs from the reviewed gateway configuration",
        ));
    }
    super::ensure_private_dir(dir)?;
    let temporary = dir.join(format!("models-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut target = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        target.write_all(CATALOG.as_bytes())?;
        target.sync_all()?;
        std::fs::rename(&temporary, dir.join("models.json"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Pi creates `{}` during startup. Preserve that resume state, but never admit
/// a credential-bearing or unreadable store left by a previous worker model.
fn require_empty_auth(dir: &Path) -> Result<()> {
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join("auth.json"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(auth_refusal()),
    };
    if !file.metadata()?.is_file() {
        return Err(auth_refusal());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file).take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(auth_refusal());
    }
    let empty = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|value| value.as_object().map(|object| object.is_empty()))
        == Some(true);
    if !empty {
        return Err(auth_refusal());
    }
    Ok(())
}

fn auth_refusal() -> Error {
    Error::rejected("AgenticOS worker requires an absent or empty auth store; use a fresh worker directory without deleting the existing login")
}
