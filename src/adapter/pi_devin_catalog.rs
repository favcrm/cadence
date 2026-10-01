//! Metadata-only bootstrap for an offline worker private Devin catalog.
use crate::{Error, Result};
use std::path::Path;

pub(super) fn seed(_cache: &Path, _source: &Path, _model: &str, _now: u64) -> Result<()> {
    Ok(())
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
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&before).unwrap(), catalog());
        std::fs::remove_file(&source).unwrap();
        seed(&cache, &source, "devin/swe-2-high", NOW).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), before);
        assert!(!cache.join("auth.json").exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(target).unwrap().permissions().mode() & 0o777, 0o600);
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
            assert!(seed(&cache, &source, "devin/swe-2-high", NOW).is_err(), "case {change}");
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
    fn concurrent_seeders_install_one_complete_snapshot() {
        let (_dir, source, cache) = fixture();
        std::thread::scope(|scope| {
            for _ in 0..4 { scope.spawn(|| seed(&cache, &source, "devin/swe-2-high", NOW).unwrap()); }
        });
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(cache.join("pi-devin/models.json")).unwrap()).unwrap(), catalog());
    }
}
