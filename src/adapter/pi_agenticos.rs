//! A credentialless, reviewed catalog for hosted AgenticOS workers.
use crate::{Error, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const CATALOG: &str = include_str!("pi_agenticos_models.json");

/// Selecting this provider is still subject to the operator's model allowlist.
/// Require the reviewed catalog in the daemon's own config, then write our
/// literal credentialless projection, never arbitrary provider keys or auth.
///
/// `model` is a Pi model spec (`agenticos/<catalog id>`). The catalog's
/// `default` entry is an alias the AgenticOS gateway resolves against its
/// DB-owned `generate_text` default, so this path never pins the platform's
/// default model; a concrete catalog id stays available as an explicit pin.
pub(super) fn seed(dir: &Path, master: &Path, model: &str) -> Result<()> {
    let reviewed: Value = serde_json::from_str(CATALOG).expect("committed AgenticOS catalog");
    if !offers(&reviewed, model) {
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

/// Whether the reviewed catalog defines `model` (`agenticos/<catalog id>`).
/// The accepted set is the catalog data itself — no model id is a code
/// constant here: `default` is the gateway-resolved alias, every other id is
/// an explicit pin the operator names.
fn offers(reviewed: &Value, model: &str) -> bool {
    let Some(id) = model.strip_prefix("agenticos/") else {
        return false;
    };
    reviewed["providers"]["agenticos"]["models"]
        .as_array()
        .is_some_and(|models| models.iter().any(|entry| entry["id"].as_str() == Some(id)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reviewed() -> Value {
        serde_json::from_str(CATALOG).expect("committed catalog")
    }

    #[test]
    fn the_reviewed_catalog_exposes_the_gateway_default_alias() {
        let reviewed = reviewed();
        let provider = &reviewed["providers"]["agenticos"];
        assert_eq!(provider["baseUrl"], "http://gateway.internal/v1");
        let ids: Vec<&str> = provider["models"]
            .as_array()
            .expect("model list")
            .iter()
            .map(|model| model["id"].as_str().expect("model id"))
            .collect();
        // The catalog's default entry is the DB-owned alias, not a model.
        assert_eq!(ids.first().copied(), Some("default"));
        assert!(offers(&reviewed, "agenticos/default"));
        // Rollback stays possible by naming the legacy pin explicitly.
        assert!(offers(&reviewed, "agenticos/z-ai/glm-5.3-flash"));
        // Only reviewed catalog ids launch on this credentialless path.
        assert!(!offers(&reviewed, "default"));
        assert!(!offers(&reviewed, "agenticos/"));
        assert!(!offers(&reviewed, "agenticos/some-model"));
        assert!(!offers(&reviewed, "openrouter/z-ai/glm-5.3-flash"));
    }

    #[test]
    fn seed_admits_the_alias_and_the_legacy_pin_only() {
        let root = tempfile::tempdir().unwrap();
        let master = root.path().join("master");
        std::fs::create_dir(&master).unwrap();
        std::fs::write(master.join("models.json"), CATALOG).unwrap();
        let worker = root.path().join("worker");

        seed(&worker, &master, "agenticos/default").expect("alias seeds");
        assert_eq!(
            std::fs::read(worker.join("models.json")).unwrap(),
            CATALOG.as_bytes()
        );
        // An explicit legacy pin still seeds — it is compatibility, not the
        // platform default.
        seed(&worker, &master, "agenticos/z-ai/glm-5.3-flash").expect("legacy pin seeds");
        // The current DB default is reached through the alias, never by
        // naming a concrete model in Cadence.
        let err = seed(
            &worker,
            &master,
            "agenticos/deepseek/deepseek-v4.1-flash-1017",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unsupported AgenticOS worker model"), "{err}");
    }
}
