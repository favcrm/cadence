//! Metadata-only bootstrap for an offline worker private Devin catalog.
use crate::{Error, Result};
use std::path::Path;

use serde_json::Value;
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Component;

const LIMIT: u64 = 1_048_576;
const MAX_AGE: u64 = 21_600_000;
fn refusal() -> Error {
    Error::rejected("Offline Devin catalog unavailable or invalid; refresh the operator catalog and use a valid private worker cache")
}
fn cname(name: &str) -> Result<CString> {
    CString::new(name).map_err(|_| refusal())
}
fn open_at(dir: &File, name: &str, flags: i32, mode: u32) -> std::io::Result<File> {
    let name =
        CString::new(name).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // Each operation is anchored to an already opened directory. NOFOLLOW
    // prevents exchanging any path component for a symlink during startup.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn directory(path: &Path) -> Result<File> {
    if !path.is_absolute() {
        return Err(refusal());
    }
    let mut dir = File::open("/")?;
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => {
                dir = open_at(
                    &dir,
                    name.to_str().ok_or_else(refusal)?,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                    0,
                )
                .map_err(|_| refusal())?;
            }
            _ => return Err(refusal()),
        }
    }
    Ok(dir)
}
fn read(dir: &File, name: &str) -> Result<Option<Vec<u8>>> {
    let mut file = match open_at(dir, name, libc::O_RDONLY, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(refusal()),
    };
    if !file.metadata()?.is_file() {
        return Err(refusal());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(refusal());
    }
    Ok(Some(bytes))
}
fn keys(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
}
fn text(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        !text.is_empty()
            && text.len() <= 512
            && text.chars().all(|c| c.is_ascii_graphic() || c == ' ')
    })
}
fn optional_text(value: &Value, key: &str) -> bool {
    value.get(key).is_none_or(text)
}
fn validate(bytes: &[u8], model: &str, now: u64) -> Result<()> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| refusal())?;
    let uid = model.strip_prefix("devin/").ok_or_else(refusal)?;
    let fetched = value["fetchedAt"].as_u64().ok_or_else(refusal)?;
    if !keys(&value, &["version", "fetchedAt", "catalog"])
        || value["version"] != 1
        || fetched > now
        || now - fetched > MAX_AGE
        || !keys(&value["catalog"], &["families"])
    {
        return Err(refusal());
    }
    let families = value["catalog"]["families"]
        .as_array()
        .ok_or_else(refusal)?;
    if families.is_empty() || families.len() > 256 {
        return Err(refusal());
    }
    let mut selected = false;
    let mut seen = std::collections::HashSet::new();
    for family in families {
        if !keys(
            family,
            &["family_label", "family_uid", "slug", "aliases", "variants"],
        ) || !["family_label", "family_uid", "slug"]
            .iter()
            .all(|key| text(&family[*key]))
            || !family.get("aliases").is_none_or(|aliases| {
                aliases
                    .as_array()
                    .is_some_and(|a| a.len() <= 32 && a.iter().all(text))
            })
        {
            return Err(refusal());
        }
        let variants = family["variants"].as_array().ok_or_else(refusal)?;
        if variants.is_empty() || variants.len() > 128 {
            return Err(refusal());
        }
        for variant in variants {
            if !keys(
                variant,
                &[
                    "model_uid",
                    "label",
                    "cost_summary",
                    "cost_tier",
                    "max_context_tokens",
                    "max_output_tokens",
                    "is_new",
                    "is_beta",
                ],
            ) || !text(&variant["model_uid"])
                || !text(&variant["label"])
                || !optional_text(variant, "cost_summary")
                || !optional_text(variant, "cost_tier")
                || !["max_context_tokens", "max_output_tokens"]
                    .iter()
                    .all(|key| {
                        variant.get(*key).is_none_or(|v| {
                            v.as_u64()
                                .is_some_and(|n| n > 0 && n <= 9_007_199_254_740_991)
                        })
                    })
                || !["is_new", "is_beta"]
                    .iter()
                    .all(|key| variant.get(*key).is_none_or(Value::is_boolean))
            {
                return Err(refusal());
            }
            let id = variant["model_uid"].as_str().ok_or_else(refusal)?;
            if !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                || !seen.insert(id)
            {
                return Err(refusal());
            }
            selected |= id == uid;
        }
    }
    if !selected {
        return Err(refusal());
    }
    Ok(())
}

/// Seed before spawning, never exposing the operator cache to the worker.
/// Existing valid private snapshots win. Offline freshness is deliberately
/// fail-closed at six hours; neither seeding nor reuse renews fetchedAt.
pub(super) fn seed(cache: &Path, source: &Path, model: &str, now: u64) -> Result<()> {
    let cache = directory(cache)?;
    let name = cname("pi-devin")?;
    let created = unsafe { libc::mkdirat(cache.as_raw_fd(), name.as_ptr(), 0o700) };
    if created < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
        return Err(refusal());
    }
    let dir = open_at(&cache, "pi-devin", libc::O_RDONLY | libc::O_DIRECTORY, 0)
        .map_err(|_| refusal())?;
    if unsafe { libc::fchmod(dir.as_raw_fd(), 0o700) } < 0 {
        return Err(refusal());
    }
    if let Some(bytes) = read(&dir, "models.json")? {
        return validate(&bytes, model, now);
    }
    let parent = directory(source.parent().ok_or_else(refusal)?)?;
    let bytes = read(
        &parent,
        source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(refusal)?,
    )?
    .ok_or_else(refusal)?;
    validate(&bytes, model, now)?;
    let temporary = format!("catalog-{}.tmp", uuid::Uuid::new_v4());
    let temporary_c = cname(&temporary)?;
    let target = cname("models.json")?;
    let result = (|| -> Result<()> {
        let mut file = open_at(
            &dir,
            &temporary,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
        .map_err(|_| refusal())?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        // Atomic no-clobber install. A concurrent winner is reread and validated.
        if unsafe {
            libc::linkat(
                dir.as_raw_fd(),
                temporary_c.as_ptr(),
                dir.as_raw_fd(),
                target.as_ptr(),
                0,
            )
        } < 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(refusal());
        }
        validate(&read(&dir, "models.json")?.ok_or_else(refusal)?, model, now)?;
        dir.sync_all()?;
        Ok(())
    })();
    unsafe {
        libc::unlinkat(dir.as_raw_fd(), temporary_c.as_ptr(), 0);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const NOW: u64 = 1_790_835_000_000;
    fn catalog() -> serde_json::Value {
        json!({"version":1,"fetchedAt":NOW,"catalog":{"families":[{
            "family_label":"SWE-2","family_uid":"swe-2","slug":"swe-2","aliases":["swe"],
            "variants":[{"model_uid":"swe-2-high","label":"SWE-2 High"},
            {"model_uid":"swe-2-medium","label":"SWE-2 Medium"},
            {"model_uid":"swe-2-max","label":"SWE-2 Max"}]}]}})
    }
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("operator.json");
        std::fs::write(&source, catalog().to_string()).unwrap();
        let cache = dir.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        (dir, source, cache)
    }
    #[test]
    fn seeds_only_catalog_and_preserves_private_existing_snapshot() {
        let (_dir, source, cache) = fixture();
        seed(&cache, &source, "devin/swe-2-high", NOW).unwrap();
        let target = cache.join("pi-devin/models.json");
        let before = std::fs::read(&target).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&before).unwrap(),
            catalog()
        );
        std::fs::remove_file(&source).unwrap();
        seed(&cache, &source, "devin/swe-2-high", NOW).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), before);
        assert!(!cache.join("auth.json").exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn refuses_invalid_stale_future_unknown_model_and_secret_fields() {
        for change in 0..6 {
            let (_dir, source, cache) = fixture();
            let mut doc = catalog();
            match change {
                0 => doc["version"] = json!(2),
                1 => doc["fetchedAt"] = json!(NOW - 21_600_001),
                2 => doc["fetchedAt"] = json!(NOW + 1),
                3 => doc["catalog"]["families"] = json!([]),
                4 => doc["catalog"]["families"][0]["apiKey"] = json!("synthetic-secret"),
                _ => doc["catalog"]["families"][0]["variants"][0]["model_uid"] = json!("unknown"),
            }
            std::fs::write(&source, doc.to_string()).unwrap();
            assert!(
                seed(&cache, &source, "devin/swe-2-high", NOW).is_err(),
                "case {change}"
            );
            assert!(!cache.join("pi-devin/models.json").exists());
        }
    }
    #[test]
    fn refuses_missing_oversized_and_symlink_sources_or_targets() {
        let (dir, source, cache) = fixture();
        std::fs::remove_file(&source).unwrap();
        assert!(seed(&cache, &source, "devin/swe-2-high", NOW).is_err());
        std::fs::write(&source, vec![b' '; 1_048_577]).unwrap();
        assert!(seed(&cache, &source, "devin/swe-2-high", NOW).is_err());
        std::fs::remove_file(&source).unwrap();
        let real = dir.path().join("real.json");
        std::fs::write(&real, catalog().to_string()).unwrap();
        std::os::unix::fs::symlink(&real, &source).unwrap();
        assert!(seed(&cache, &source, "devin/swe-2-high", NOW).is_err());
        std::fs::remove_file(&source).unwrap();
        std::fs::write(&source, catalog().to_string()).unwrap();
        std::os::unix::fs::symlink(dir.path(), cache.join("pi-devin")).unwrap();
        assert!(seed(&cache, &source, "devin/swe-2-high", NOW).is_err());
    }
    #[test]
    fn refuses_symlinked_ancestor_and_existing_invalid_private_cache() {
        let (dir, source, cache) = fixture();
        let alias = dir.path().join("cache-alias");
        std::os::unix::fs::symlink(&cache, &alias).unwrap();
        assert!(seed(&alias, &source, "devin/swe-2-high", NOW).is_err());
        std::fs::create_dir(cache.join("pi-devin")).unwrap();
        std::fs::write(cache.join("pi-devin/models.json"), b"invalid").unwrap();
        assert!(seed(&cache, &source, "devin/swe-2-high", NOW).is_err());
        assert_eq!(
            std::fs::read(cache.join("pi-devin/models.json")).unwrap(),
            b"invalid"
        );
    }

    #[test]
    fn concurrent_seeders_install_one_complete_snapshot() {
        let (_dir, source, cache) = fixture();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| seed(&cache, &source, "devin/swe-2-high", NOW).unwrap());
            }
        });
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(cache.join("pi-devin/models.json")).unwrap()
            )
            .unwrap(),
            catalog()
        );
    }
}
