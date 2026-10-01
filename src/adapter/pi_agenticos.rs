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
pub(super) fn seed(dir: &Path, operator: Option<&Path>, model: &str) -> Result<()> {
    if model != MODEL {
        return Err(Error::rejected("unsupported AgenticOS worker model"));
    }
    let source = operator
        .ok_or_else(|| Error::rejected("AgenticOS worker requires the reviewed operator catalog"))?
        .join("models.json");
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
