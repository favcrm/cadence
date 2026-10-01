//! Metadata-only bootstrap for an offline worker private Devin catalog.
use crate::{Error, Result};
use std::path::Path;

use serde_json::Value;
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Component;

// Reject duplicate members before metadata validation; serde_json::Value
// alone would silently retain the last member and hide malformed input.
struct UniqueValue(Value);
impl<'de> serde::Deserialize<'de> for UniqueValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON with unique object members")
            }
            fn visit_bool<E: serde::de::Error>(
                self,
                v: bool,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| UniqueValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(v) = a.next_element::<UniqueValue>()? {
                    values.push(v.0);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = a.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate object member"));
                    }
                    values.insert(key, a.next_value::<UniqueValue>()?.0);
                }
                Ok(UniqueValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

// Input byte cap, enforced before parsing, and structure caps. Basis: the
// live catalog is about 270 KiB with about 54 families and a largest family
// of 460 variants, so 2 MiB is about 7x headroom (the snapshot is worker-writable and re-read
// at each launch, so it stays tight), 1024 families about 19x and 4096
// variants per family about 9x.
const LIMIT: u64 = 2 * 1_048_576;
const MAX_FAMILIES: usize = 1024;
const MAX_VARIANTS: usize = 4096;
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
// Display-only strings (labels, cost text, aliases) accept Unicode such as
// the live catalog's U+00B7 separator. Refused: control characters,
// U+2028/2029 line/paragraph separators, and bidi overrides (U+202A-202E,
// U+2066-2069). Still at most 512 bytes. (CAD-992)
fn is_display_char(c: char) -> bool {
    if c.is_control() {
        return false;
    }
    !matches!(
        c,
        '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}
fn display(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|t| !t.is_empty() && t.len() <= 512 && t.chars().all(is_display_char))
}
fn optional_display(value: &Value, key: &str) -> bool {
    value.get(key).is_none_or(|v| v.is_null() || display(v))
}
/// Wire `description`: same display charset, still at most 4096 bytes.
/// Empty is allowed; the charset excludes controls and bidi overrides
/// (previously only NUL was rejected). (CAD-992)
fn description_text(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| s.len() <= 4096 && s.chars().all(is_display_char))
}
fn validate(bytes: &[u8], model: &str, now: u64) -> Result<Vec<u8>> {
    let mut value = serde_json::from_slice::<UniqueValue>(bytes)
        .map_err(|_| refusal())?
        .0;
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
    if families.is_empty() || families.len() > MAX_FAMILIES {
        return Err(refusal());
    }
    let mut selected = false;
    let mut seen = std::collections::HashSet::new();
    for family in families {
        if !keys(
            family,
            &["family_label", "family_uid", "slug", "aliases", "variants"],
        ) || !display(&family["family_label"])
            || !["family_uid", "slug"].iter().all(|key| text(&family[*key]))
            || !family.get("aliases").is_none_or(|aliases| {
                aliases
                    .as_array()
                    .is_some_and(|a| a.len() <= 32 && a.iter().all(display))
            })
        {
            return Err(refusal());
        }
        let variants = family["variants"].as_array().ok_or_else(refusal)?;
        if variants.is_empty() || variants.len() > MAX_VARIANTS {
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
                    "description",
                    "max_context_tokens",
                    "max_output_tokens",
                    "is_new",
                    "is_beta",
                ],
            ) || !text(&variant["model_uid"])
                || !display(&variant["label"])
                || !optional_display(variant, "cost_summary")
                || !optional_display(variant, "cost_tier")
                || !variant
                    .get("description")
                    .is_none_or(|v| v.is_null() || description_text(v))
                || !["max_context_tokens", "max_output_tokens"]
                    .iter()
                    .all(|key| {
                        variant.get(*key).is_none_or(|v| {
                            v.is_null()
                                || v.as_u64()
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
    // The CLI wire catalog may carry nullable optional display metadata.
    // pi-devin0.2.1 rejects null cost fields in its cache; omit those fields
    // while preserving family/variant membership and original fetchedAt.
    for family in value["catalog"]["families"]
        .as_array_mut()
        .ok_or_else(refusal)?
    {
        for variant in family["variants"].as_array_mut().ok_or_else(refusal)? {
            let object = variant.as_object_mut().ok_or_else(refusal)?;
            for key in [
                "cost_tier",
                "cost_summary",
                "description",
                "max_context_tokens",
                "max_output_tokens",
            ] {
                if object.get(key).is_some_and(Value::is_null) {
                    object.remove(key);
                }
            }
        }
    }
    serde_json::to_vec(&value).map_err(|_| refusal())
}

fn usable_private_snapshot(bytes: &[u8], model: &str, now: u64) -> Result<()> {
    let normalized = validate(bytes, model, now)?;
    // Worker-owned snapshots must already match the pinned provider cache
    // contract. Source normalization never authorizes overwriting them.
    if serde_json::from_slice::<Value>(bytes).ok()
        != serde_json::from_slice::<Value>(&normalized).ok()
    {
        return Err(refusal());
    }
    Ok(())
}
fn install_snapshot(dir: &File, temporary: &str, model: &str, now: u64) -> Result<()> {
    let temporary = cname(temporary)?;
    let target = cname("models.json")?;
    if unsafe {
        libc::linkat(
            dir.as_raw_fd(),
            temporary.as_ptr(),
            dir.as_raw_fd(),
            target.as_ptr(),
            0,
        )
    } < 0
        && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(refusal());
    }
    usable_private_snapshot(&read(dir, "models.json")?.ok_or_else(refusal)?, model, now)?;
    dir.sync_all()?;
    Ok(())
}
/// Replace a stale invalid private snapshot with validated operator bytes
/// already staged at `temporary` (0600, O_EXCL). A concurrent fix that is
/// now valid wins and is kept; only a still-invalid snapshot is replaced
/// via atomic rename (which replaces a regular file in place, never
/// following a symlink). Missing target renames into place as a move.
fn replace_invalid_snapshot(dir: &File, temporary: &str, model: &str, now: u64) -> Result<()> {
    if let Some(current) = read(dir, "models.json")? {
        if usable_private_snapshot(&current, model, now).is_ok() {
            return Ok(());
        }
    }
    let temporary = cname(temporary)?;
    let target = cname("models.json")?;
    if unsafe {
        libc::renameat(
            dir.as_raw_fd(),
            temporary.as_ptr(),
            dir.as_raw_fd(),
            target.as_ptr(),
        )
    } < 0
    {
        return Err(refusal());
    }
    usable_private_snapshot(&read(dir, "models.json")?.ok_or_else(refusal)?, model, now)?;
    dir.sync_all()?;
    Ok(())
}

/// Seed before spawning, never exposing the operator cache to the worker.
/// Existing valid private snapshots win without touching the operator file.
/// A stale invalid snapshot does not block forever: it is re-seeded from a
/// valid operator file. Offline freshness is deliberately fail-closed at six
/// hours; neither seeding nor reuse renews fetchedAt.
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
    let had_invalid = match read(&dir, "models.json")? {
        Some(bytes) => {
            if usable_private_snapshot(&bytes, model, now).is_ok() {
                return Ok(());
            }
            // Invalid stale snapshot: fall through to re-seed from the
            // operator file. Valid snapshots win above and never touch the
            // operator file.
            true
        }
        None => false,
    };
    let parent = directory(source.parent().ok_or_else(refusal)?)?;
    let bytes = read(
        &parent,
        source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(refusal)?,
    )?
    .ok_or_else(refusal)?;
    let bytes = validate(&bytes, model, now)?;
    let temporary = format!("catalog-{}.tmp", uuid::Uuid::new_v4());
    let temporary_c = cname(&temporary)?;
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
        if had_invalid {
            // Stale invalid snapshot: a now-valid concurrent fix wins and
            // is kept; otherwise atomically replace the stale file.
            replace_invalid_snapshot(&dir, &temporary, model, now)
        } else {
            // Atomic no-clobber install. Reread winners using the same
            // private usability predicate as startup's existing-cache path.
            install_snapshot(&dir, &temporary, model, now)
        }
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

    fn family(uid: &str, count: usize) -> Value {
        let variants: Vec<Value> = (0..count)
            .map(|i| json!({"model_uid": format!("{uid}-v{i}"), "label": format!("{uid} v{i}")}))
            .collect();
        json!({"family_label":uid,"family_uid":uid,"slug":uid,"variants":variants})
    }
    fn seed_doc(doc: &Value) -> Result<()> {
        let (_dir, source, cache) = fixture();
        std::fs::write(&source, doc.to_string()).unwrap();
        seed(&cache, &source, "devin/swe-2-high", NOW)
    }
    fn with_families(mut extra: Vec<Value>) -> Value {
        let mut doc = catalog();
        doc["catalog"]["families"]
            .as_array_mut()
            .unwrap()
            .append(&mut extra);
        doc
    }
    #[test]
    fn accepts_live_sized_family_and_refuses_just_over_each_bound() {
        // CAD-993: the live catalog's Fusion family has 460 variants.
        seed_doc(&with_families(vec![family("fusion", 460)])).unwrap();
        seed_doc(&with_families(vec![family("big", MAX_VARIANTS)])).unwrap();
        assert!(seed_doc(&with_families(vec![family("big", MAX_VARIANTS + 1)])).is_err());
        let many = |n: usize| (0..n - 1).map(|i| family(&format!("f{i}"), 1)).collect();
        seed_doc(&with_families(many(MAX_FAMILIES))).unwrap();
        assert!(seed_doc(&with_families(many(MAX_FAMILIES + 1))).is_err());
    }
    #[test]
    fn accepts_trimmed_sample_with_real_live_string_shapes() {
        // Real live shapes: a middle-dot in cost_summary, the real cost tiers,
        // and nullable description / max_output_tokens / cost_summary.
        let mut fusion = family("fusion", 460);
        for (i, v) in fusion["variants"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            v["cost_summary"] =
                json!("$0.5 / 1M Input \u{b7} $0.1 / 1M Cached input \u{b7} $2 / 1M Output");
            v["cost_tier"] = json!(["High cost", "Med cost", "Low cost", "Free"][i % 4]);
            v["description"] = if i % 2 == 0 {
                Value::Null
            } else {
                json!("Synthetic description.")
            };
            v["max_context_tokens"] = json!(262000);
            v["max_output_tokens"] = if i % 2 == 0 {
                Value::Null
            } else {
                json!(64000)
            };
            v["is_new"] = json!(i % 3 == 0);
            v["is_beta"] = json!(false);
        }
        fusion["aliases"] = json!(["fusion-alias"]);
        let mut sparse = family("sparse", 2);
        sparse["variants"][0]["cost_summary"] = Value::Null;
        sparse["variants"][1]["cost_tier"] = Value::Null;
        let doc = with_families(vec![fusion, sparse]);
        let (_dir, source, cache) = fixture();
        std::fs::write(&source, doc.to_string()).unwrap();
        seed(&cache, &source, "devin/fusion-v459", NOW).unwrap();
        assert!(cache.join("pi-devin/models.json").exists());
    }
    #[test]
    fn display_fields_allow_unicode_but_never_control_and_ids_stay_ascii() {
        let with = |f: fn(&mut Value)| {
            let mut doc = catalog();
            f(&mut doc);
            seed_doc(&doc)
        };
        let v = |d: &mut Value| d["catalog"]["families"][0]["variants"][0].clone();
        let _ = v;
        assert!(with(|d| d["catalog"]["families"][0]["family_label"] = json!("S\u{b7}WE")).is_ok());
        assert!(with(|d| d["catalog"]["families"][0]["aliases"] = json!(["a\u{b7}b"])).is_ok());
        assert!(
            with(|d| d["catalog"]["families"][0]["variants"][0]["label"] = json!("H\u{e9}"))
                .is_ok()
        );
        assert!(
            with(|d| d["catalog"]["families"][0]["variants"][0]["cost_tier"] = json!("a\nb"))
                .is_err()
        );
        assert!(
            with(|d| d["catalog"]["families"][0]["variants"][0]["label"] = json!("a\u{7}b"))
                .is_err()
        );
        assert!(with(|d| d["catalog"]["families"][0]["slug"] = json!("sl\u{b7}ug")).is_err());
        assert!(with(|d| d["catalog"]["families"][0]["family_uid"] = json!("f\u{b7}")).is_err());
        assert!(with(
            |d| d["catalog"]["families"][0]["variants"][0]["model_uid"] = json!("m\u{b7}")
        )
        .is_err());
    }
    // Operator check: CADENCE_REAL_CATALOG=<copy of the live models.json>
    // CADENCE_REAL_MODEL=devin/<uid> cargo test --lib real_catalog -- --ignored
    #[test]
    #[ignore]
    fn real_catalog_copy_is_accepted() {
        let src = std::env::var("CADENCE_REAL_CATALOG").unwrap();
        let model = std::env::var("CADENCE_REAL_MODEL").unwrap();
        let bytes = std::fs::read(&src).unwrap();
        let fetched = serde_json::from_slice::<Value>(&bytes).unwrap()["fetchedAt"]
            .as_u64()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let source = dir.path().join("copy.json");
        std::fs::write(&source, &bytes).unwrap();
        seed(&cache, &source, &model, fetched).unwrap();
    }
    #[test]
    fn padded_valid_catalog_is_accepted_at_the_byte_cap_and_refused_past_it() {
        for (extra, ok) in [(0usize, true), (1, false)] {
            let (_dir, source, cache) = fixture();
            let mut padded = catalog().to_string().into_bytes();
            padded.resize(LIMIT as usize + extra, b' ');
            std::fs::write(&source, &padded).unwrap();
            assert_eq!(
                seed(&cache, &source, "devin/swe-2-high", NOW).is_ok(),
                ok,
                "extra {extra}"
            );
        }
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
        // A VALID catalog padded with trailing whitespace past the cap: only
        // the byte cap can refuse it, not JSON parsing.
        let mut padded = catalog().to_string().into_bytes();
        padded.resize(LIMIT as usize + 1, b' ');
        std::fs::write(&source, &padded).unwrap();
        assert!(seed(&cache, &source, "devin/swe-2-high", NOW).is_err());
        std::fs::remove_file(&source).unwrap();
        let real = dir.path().join("real.json");
        std::fs::write(&real, catalog().to_string()).unwrap();
        std::os::unix::fs::symlink(&real, &source).unwrap();
        assert!(seed(&cache, &source, "devin/swe-2-high", NOW).is_err());
        std::fs::remove_file(&source).unwrap();
        std::fs::write(&source, catalog().to_string()).unwrap();
        // Failed reads create only the empty private directory. Remove this
        // owned fixture directory before replacing it with a hostile symlink.
        std::fs::remove_dir(cache.join("pi-devin")).unwrap();
        std::os::unix::fs::symlink(dir.path(), cache.join("pi-devin")).unwrap();
        assert!(seed(&cache, &source, "devin/swe-2-high", NOW).is_err());
    }
    #[test]
    fn refuses_symlinked_ancestor() {
        let (dir, source, cache) = fixture();
        let alias = dir.path().join("cache-alias");
        std::os::unix::fs::symlink(&cache, &alias).unwrap();
        assert!(seed(&alias, &source, "devin/swe-2-high", NOW).is_err());
    }
    #[test]
    fn invalid_stale_private_snapshot_reseeds_from_valid_operator() {
        let (_dir, source, cache) = fixture();
        std::fs::create_dir(cache.join("pi-devin")).unwrap();
        std::fs::write(cache.join("pi-devin/models.json"), b"invalid").unwrap();
        // CAD-992: a stale invalid snapshot must not block forever — a valid
        // operator file re-seeds and replaces it.
        seed(&cache, &source, "devin/swe-2-high", NOW).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(cache.join("pi-devin/models.json")).unwrap()
            )
            .unwrap(),
            catalog()
        );
        // The re-seeded valid snapshot now wins without the operator file.
        std::fs::remove_file(&source).unwrap();
        seed(&cache, &source, "devin/swe-2-high", NOW).unwrap();
    }
    #[test]
    fn invalid_stale_private_with_invalid_operator_still_refuses() {
        let (_dir, source, cache) = fixture();
        std::fs::create_dir(cache.join("pi-devin")).unwrap();
        std::fs::write(cache.join("pi-devin/models.json"), b"invalid").unwrap();
        let mut bad = catalog();
        bad["version"] = json!(2);
        std::fs::write(&source, bad.to_string()).unwrap();
        assert!(seed(&cache, &source, "devin/swe-2-high", NOW).is_err());
        assert_eq!(
            std::fs::read(cache.join("pi-devin/models.json")).unwrap(),
            b"invalid"
        );
    }
    #[test]
    fn real_shaped_cost_summary_with_middle_dot_passes() {
        let (_dir, source, cache) = fixture();
        let mut doc = catalog();
        // Real-shaped entry mirroring ~/.cache/pi-devin/models.json:
        // cost_summary carries U+00B7 MIDDLE DOT separators.
        doc["catalog"]["families"][0]["variants"][0]["cost_summary"] =
            json!("$0.5 / 1M Input \u{00b7} $0.1 / 1M Cached input \u{00b7} $2 / 1M Output");
        doc["catalog"]["families"][0]["variants"][0]["description"] =
            json!("Automatically balances quality and cost");
        std::fs::write(&source, doc.to_string()).unwrap();
        seed(&cache, &source, "devin/swe-2-high", NOW).unwrap();
        let saved: Value =
            serde_json::from_slice(&std::fs::read(cache.join("pi-devin/models.json")).unwrap())
                .unwrap();
        assert_eq!(saved, doc);
    }
    #[test]
    fn control_and_bidi_overrides_in_display_are_refused() {
        for bad in [
            "cost \u{0000} summary",
            "cost\nsummary",
            "cost\u{007f}summary",
            "cost\u{2028}summary",
            "cost\u{2029}summary",
            "cost\u{202A}summary",
            "cost\u{202E}summary",
            "cost\u{2066}summary",
            "cost\u{2069}summary",
        ] {
            let (_dir, source, cache) = fixture();
            let mut doc = catalog();
            doc["catalog"]["families"][0]["variants"][0]["cost_summary"] = json!(bad);
            std::fs::write(&source, doc.to_string()).unwrap();
            assert!(
                seed(&cache, &source, "devin/swe-2-high", NOW).is_err(),
                "accepted {bad:?}"
            );
            assert!(!cache.join("pi-devin/models.json").exists());
        }
        // Same guards apply to label and description.
        for (key, bad) in [
            ("label", "x\u{202E}y"),
            ("description", "x\u{0000}y"),
            ("description", "x\u{2066}y"),
        ] {
            let (_dir, source, cache) = fixture();
            let mut doc = catalog();
            doc["catalog"]["families"][0]["variants"][0][key] = json!(bad);
            std::fs::write(&source, doc.to_string()).unwrap();
            assert!(
                seed(&cache, &source, "devin/swe-2-high", NOW).is_err(),
                "accepted {key} {bad:?}"
            );
        }
    }

    #[test]
    fn representative_wire_metadata_accepts_all_optional_types_and_normalizes_nulls() {
        let (_dir, source, cache) = fixture();
        let mut doc = catalog();
        let variant = &mut doc["catalog"]["families"][0]["variants"][0];
        variant["description"] =
            json!("Synthetic model description matching the CLI metadata contract.");
        variant["cost_tier"] = Value::Null;
        variant["cost_summary"] = json!("Synthetic display cost");
        variant["max_context_tokens"] = Value::Null;
        variant["max_output_tokens"] = Value::Null;
        variant["is_new"] = json!(false);
        variant["is_beta"] = json!(false);
        // Another variant covers non-null forms of every optional wire field.
        let other = &mut doc["catalog"]["families"][0]["variants"][1];
        other["description"] = Value::Null;
        other["cost_tier"] = json!("Synthetic tier");
        other["cost_summary"] = Value::Null;
        other["max_context_tokens"] = json!(262000);
        other["max_output_tokens"] = json!(128000);
        other["is_new"] = json!(true);
        other["is_beta"] = json!(true);
        std::fs::write(&source, doc.to_string()).unwrap();
        seed(&cache, &source, "devin/swe-2-high", NOW).unwrap();
        let saved: Value =
            serde_json::from_slice(&std::fs::read(cache.join("pi-devin/models.json")).unwrap())
                .unwrap();
        doc["catalog"]["families"][0]["variants"][0]
            .as_object_mut()
            .unwrap()
            .remove("cost_tier");
        for variant in doc["catalog"]["families"][0]["variants"]
            .as_array_mut()
            .unwrap()
        {
            let object = variant.as_object_mut().unwrap();
            object.retain(|_, value| !value.is_null());
        }
        assert_eq!(saved, doc);
        assert_eq!(saved["fetchedAt"], NOW);
    }
    #[test]
    fn distinct_workers_are_isolated_and_write_failure_leaves_no_partial_catalog() {
        let (dir, source, cache) = fixture();
        let second = dir.path().join("second");
        std::fs::create_dir(&second).unwrap();
        seed(&cache, &source, "devin/swe-2-high", NOW).unwrap();
        seed(&second, &source, "devin/swe-2-high", NOW).unwrap();
        std::fs::write(cache.join("pi-devin/models.json"), b"changed privately").unwrap();
        seed(&second, &source, "devin/swe-2-high", NOW).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(
                &std::fs::read(second.join("pi-devin/models.json")).unwrap()
            )
            .unwrap(),
            catalog()
        );
        // Deterministic ENOTDIR works for root as well; chmod is not a
        // meaningful write-failure fixture when tests run as root.
        let blocked = dir.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("pi-devin"), b"owned obstruction").unwrap();
        assert!(seed(&blocked, &source, "devin/swe-2-high", NOW).is_err());
        assert_eq!(
            std::fs::read(blocked.join("pi-devin")).unwrap(),
            b"owned obstruction"
        );
    }

    #[test]
    fn duplicate_catalog_and_nested_variant_keys_are_rejected_without_copying() {
        for nested in [false, true] {
            let (_dir, source, cache) = fixture();
            let good = catalog().to_string();
            let wire = if nested {
                good.replace(
                    "\"model_uid\":\"swe-2-high\"",
                    "\"model_uid\":\"synthetic-secret\",\"model_uid\":\"swe-2-high\"",
                )
            } else {
                good.replacen("{", "{\"catalog\":{\"apiKey\":\"synthetic-secret\"},", 1)
            };
            std::fs::write(&source, wire).unwrap();
            assert!(seed(&cache, &source, "devin/swe-2-high", NOW).is_err());
            assert!(!cache.join("pi-devin/models.json").exists());
        }
    }

    #[test]
    fn concurrent_nullable_winner_cannot_bypass_private_cache_usability() {
        let (_dir, _source, cache) = fixture();
        let private = cache.join("pi-devin");
        std::fs::create_dir(&private).unwrap();
        std::fs::write(private.join("candidate.tmp"), catalog().to_string()).unwrap();
        let mut winner = catalog();
        winner["catalog"]["families"][0]["variants"][0]["max_context_tokens"] = Value::Null;
        std::fs::write(private.join("models.json"), winner.to_string()).unwrap();
        // Deterministically model a winner installed after the initial absent
        // read and before linkat. Do not overwrite that worker-owned snapshot.
        let opened = directory(&private).unwrap();
        assert!(install_snapshot(&opened, "candidate.tmp", "devin/swe-2-high", NOW).is_err());
        assert_eq!(
            std::fs::read_to_string(private.join("models.json")).unwrap(),
            winner.to_string()
        );
        assert!(
            usable_private_snapshot(winner.to_string().as_bytes(), "devin/swe-2-high", NOW)
                .is_err()
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
