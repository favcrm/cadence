//! The pinned Pi 1.4.0 loaded-registry observation, canonical digesting and
//! the immutable installed-material comparison.
//!
//! The observer reports *what is actually registered* (names, schemas,
//! implementation/resource digests, exposure and the active set) plus its
//! post-exec seal facts. The owner:
//!
//! 1. re-canonicalizes the payload bytes and recomputes the registry digest;
//! 2. requires every observed tool to equal an approved manifest entry and
//!    requires the approved set to be complete;
//! 3. requires the active callable set to equal the observed registry;
//! 4. requires the seal facts to be the measured post-exec values;
//! 5. hashes the live installed material through `/proc/<pid>/root` and
//!    requires the profile's pinned material digest.
//!
//! A manifest file, receipt or observer claim alone is never sufficient.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::profile::{ApprovedTool, LocalProfile};
use super::strict;

const MAX_MATERIAL_ENTRIES: usize = 4096;
const MAX_MATERIAL_FILE: u64 = 64 * 1024 * 1024;
const MAX_MATERIAL_TOTAL: u64 = 512 * 1024 * 1024;

/// The observer's payload fields, in the exact set the MAC covers.
pub(crate) const PAYLOAD_FIELDS: &[&str] = &[
    "v",
    "runtime_generation",
    "node_pid",
    "seal",
    "argv_digest",
    "env_digest",
    "pi_version",
    "observer_digest",
    "tools",
    "active",
    "policy_digest",
];

/// Canonical JSON: sorted object keys, no whitespace, integers only, and the
/// escaping JavaScript's `JSON.stringify` produces for strings.
pub(crate) fn canonical(value: &serde_json::Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_value(value, &mut out, 0)?;
    Ok(out)
}

fn write_value(value: &serde_json::Value, out: &mut Vec<u8>, depth: usize) -> Result<()> {
    if depth > 16 {
        return Err(Error::rejected("canonical JSON nesting exceeds 16"));
    }
    match value {
        serde_json::Value::Null => out.extend_from_slice(b"null"),
        serde_json::Value::Bool(true) => out.extend_from_slice(b"true"),
        serde_json::Value::Bool(false) => out.extend_from_slice(b"false"),
        serde_json::Value::Number(n) => {
            if n.as_i64().is_none() && n.as_u64().is_none() {
                return Err(Error::rejected(
                    "canonical JSON refuses non-integer numbers",
                ));
            }
            out.extend_from_slice(n.to_string().as_bytes());
        }
        serde_json::Value::String(s) => write_string(s, out),
        serde_json::Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_value(item, out, depth + 1)?;
            }
            out.push(b']');
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push(b'{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_string(key, out);
                out.push(b':');
                write_value(&map[key], out, depth + 1)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn write_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{08}' => out.extend_from_slice(b"\\b"),
            '\u{0c}' => out.extend_from_slice(b"\\f"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes())
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

pub(crate) fn digest_of(payload: &serde_json::Value) -> Result<String> {
    let bytes = canonical(payload)?;
    let mut material = Vec::with_capacity(bytes.len() + 32);
    material.extend_from_slice(b"cadence.runtime.registry.v1\0");
    material.extend_from_slice(&bytes);
    Ok(super::profile::sha256_digest(&material))
}

/// The MAC input is fixed ASCII, so the JS observer and the Rust owner cannot
/// disagree about JSON canonicalization for authentication.
pub(crate) fn mac_material(
    generation: u64,
    challenge_hex: &str,
    counter: u64,
    registry_digest: &str,
) -> Vec<u8> {
    format!(
        "cadence.runtime.registry/1\n{generation}\n{challenge_hex}\n{counter}\n{registry_digest}\n"
    )
    .into_bytes()
}

/// A verified registry observation.
#[derive(Debug)]
pub(crate) struct Observation {
    pub(crate) generation: u64,
    pub(crate) node_pid: u32,
    pub(crate) counter: u64,
    pub(crate) registry_digest: String,
    pub(crate) payload: serde_json::Value,
}

/// A verified registry observation proof. Only [`verify_payload`] constructs
/// it, after checking every profile-bound field and the stable approved
/// projection against the profile pin. The full dynamic digest remains the
/// HMAC/observation/loaded-receipt digest; the stable digest is the static
/// profile projection.
#[derive(Debug)]
pub(crate) struct VerifiedRegistry {
    generation: u64,
    dynamic_digest: String,
    stable_digest: String,
}

impl VerifiedRegistry {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn dynamic_digest(&self) -> &str {
        &self.dynamic_digest
    }
    pub(crate) fn stable_digest(&self) -> &str {
        &self.stable_digest
    }
}

/// Verify the full observed payload against the root profile. Returns the
/// recomputed full dynamic registry digest plus the stable approved projection
/// digest, which must equal the static profile pin.
pub(crate) fn verify_payload(
    payload: &serde_json::Value,
    profile: &LocalProfile,
    expected_generation: u64,
) -> Result<VerifiedRegistry> {
    let m = strict::object(payload, PAYLOAD_FIELDS)?;
    if strict::safe_u64(m, "v")? != 1 {
        return Err(Error::rejected("observation v must be exactly 1"));
    }
    let generation = strict::safe_u64(m, "runtime_generation")?;
    if generation != expected_generation {
        return Err(Error::rejected(
            "observation names a different runtime generation",
        ));
    }
    let node_pid = strict::safe_u64(m, "node_pid")?;
    if node_pid != 1 {
        return Err(Error::rejected("observer must be the measured guest PID 1"));
    }
    if strict::string(m, "pi_version")? != "1.4.0" {
        return Err(Error::rejected("observer is not the pinned Pi 1.4.0 graph"));
    }
    let seal = strict::object(
        m.get("seal")
            .ok_or_else(|| Error::rejected("seal facts missing"))?,
        &["dumpable", "core", "nnp", "seccomp", "caps"],
    )?;
    if strict::safe_u64(seal, "dumpable")? != 0
        || strict::safe_u64(seal, "core")? != 0
        || strict::safe_u64(seal, "nnp")? != 1
        || strict::safe_u64(seal, "seccomp")? != 2
    {
        return Err(Error::rejected(
            "post-exec seal facts are not dumpable=0/core=0/nnp=1/seccomp=2",
        ));
    }
    let caps = strict::string(seal, "caps")?;
    if caps.len() != 16
        || !caps.bytes().all(|b| b.is_ascii_hexdigit())
        || caps != "0000000000000000"
    {
        return Err(Error::rejected("post-exec capability set is not empty"));
    }
    let argv_digest = strict::string(m, "argv_digest")?;
    strict::digest(argv_digest)?;
    let env_digest = strict::string(m, "env_digest")?;
    strict::digest(env_digest)?;
    let observer_digest = strict::string(m, "observer_digest")?;
    strict::digest(observer_digest)?;
    if observer_digest != profile.runtime().observer_digest {
        return Err(Error::rejected(
            "observer implementation digest is not the pinned observer",
        ));
    }
    let policy_digest = strict::string(m, "policy_digest")?;
    strict::digest(policy_digest)?;
    if policy_digest != profile.registry().policy_digest {
        return Err(Error::rejected(
            "observed policy digest is not the approved policy",
        ));
    }

    let tools = strict::array(m, "tools")?;
    if tools.is_empty() || tools.len() > 256 {
        return Err(Error::rejected("observed tool registry is empty/oversized"));
    }
    let mut observed: Vec<ApprovedTool> = Vec::new();
    for entry in tools {
        let t = strict::object(
            entry,
            &[
                "name",
                "exposure",
                "schema_digest",
                "implementation_digest",
                "source_kind",
                "source_path",
            ],
        )?;
        let name = strict::string(t, "name")?;
        if name.is_empty() || name.len() > 128 || name.bytes().any(|b| b.is_ascii_control()) {
            return Err(Error::rejected("observed tool name is malformed"));
        }
        let exposure = strict::string(t, "exposure")?;
        if !matches!(
            exposure,
            "direct" | "model-only" | "codemode" | "deferred" | "hidden"
        ) {
            return Err(Error::rejected("observed tool exposure is unknown"));
        }
        let source_kind = strict::string(t, "source_kind")?;
        if !matches!(source_kind, "builtin" | "extension" | "package" | "mcp") {
            return Err(Error::rejected("observed tool source kind is unknown"));
        }
        let source_path = strict::string(t, "source_path")?;
        if source_path.is_empty() || source_path.len() > 512 {
            return Err(Error::rejected("observed tool source path is malformed"));
        }
        let schema_digest = strict::string(t, "schema_digest")?;
        strict::digest(schema_digest)?;
        let implementation_digest = strict::string(t, "implementation_digest")?;
        strict::digest(implementation_digest)?;
        if observed.iter().any(|existing| existing.name == name) {
            return Err(Error::rejected("duplicate observed tool name"));
        }
        observed.push(ApprovedTool {
            name: name.to_string(),
            schema_digest: schema_digest.to_string(),
            implementation_digest: implementation_digest.to_string(),
        });
    }
    let mut approved = profile.registry().approved.clone();
    observed.sort_by(|a, b| a.name.cmp(&b.name));
    approved.sort_by(|a, b| a.name.cmp(&b.name));
    if observed.len() != approved.len() {
        return Err(Error::rejected(
            "observed registry does not match the approved manifest set",
        ));
    }
    for (seen, expected) in observed.iter().zip(approved.iter()) {
        if seen.name != expected.name
            || seen.schema_digest != expected.schema_digest
            || seen.implementation_digest != expected.implementation_digest
        {
            return Err(Error::rejected(
                "observed tool material/schema differs from the approved manifest",
            ));
        }
    }
    let active = strict::array(m, "active")?;
    let mut active_set: BTreeSet<String> = BTreeSet::new();
    for entry in active {
        let name = entry
            .as_str()
            .ok_or_else(|| Error::rejected("active tool names must be strings"))?;
        if !active_set.insert(name.to_string()) {
            return Err(Error::rejected("duplicate active tool name"));
        }
    }
    let observed_names: BTreeSet<String> = observed.iter().map(|t| t.name.clone()).collect();
    if active_set != observed_names {
        return Err(Error::rejected(
            "active callable set differs from the observed registry",
        ));
    }

    let dynamic_digest = digest_of(payload)?;
    // The profile pins the stable approved-registry projection, not the full
    // dynamic payload (which legitimately includes generation/PID/seal).
    let mut projection_tools = Vec::with_capacity(observed.len());
    for tool in &observed {
        projection_tools.push(serde_json::json!({
            "name": tool.name,
            "schema_digest": tool.schema_digest,
            "implementation_digest": tool.implementation_digest,
        }));
    }
    let projection = serde_json::json!({
        "v": 1,
        "pi_version": "1.4.0",
        "tools": projection_tools,
    });
    let stable_digest = digest_of(&projection)?;
    if stable_digest != profile.registry().registry_digest {
        return Err(Error::rejected(
            "stable approved registry projection is not the profile pin",
        ));
    }
    Ok(VerifiedRegistry {
        generation: expected_generation,
        dynamic_digest,
        stable_digest,
    })
}

/// Hash the live installed material through `/proc/<pid>/root`. Expensive but
/// bounded; symlinks, special files and oversized trees refuse.
pub(crate) fn material_digest(guest_root: &Path, material_root: &str) -> Result<String> {
    let root = guest_root.join(
        material_root
            .strip_prefix('/')
            .ok_or_else(|| Error::rejected("material root must be absolute"))?,
    );
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut total: u64 = 0;
    walk(&root, &root, &mut entries, &mut total, 0)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut material = Vec::new();
    material.extend_from_slice(b"cadence.runtime.material.v1\0");
    for (rel, line) in entries {
        material.extend_from_slice(rel.as_bytes());
        material.push(0);
        material.extend_from_slice(line.as_bytes());
        material.push(b'\n');
    }
    Ok(super::profile::sha256_digest(&material))
}

fn walk(
    root: &Path,
    dir: &Path,
    entries: &mut Vec<(String, String)>,
    total: &mut u64,
    depth: usize,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if depth > 16 {
        return Err(Error::rejected("material tree is too deep"));
    }
    let listing =
        std::fs::read_dir(dir).map_err(|_| Error::unknown("installed material is unreadable"))?;
    for entry in listing {
        let entry = entry.map_err(|_| Error::unknown("installed material is unreadable"))?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|_| Error::internal("material path escaped the root"))?
            .to_string_lossy()
            .to_string();
        if entries.len() >= MAX_MATERIAL_ENTRIES {
            return Err(Error::rejected(
                "installed material exceeds the entry bound",
            ));
        }
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|_| Error::unknown("installed material metadata unreadable"))?;
        if meta.file_type().is_symlink() {
            return Err(Error::rejected("installed material contains a symlink"));
        }
        if meta.is_dir() {
            entries.push((rel, format!("dir:{:o}", meta.mode() & 0o7777)));
            walk(root, &path, entries, total, depth + 1)?;
        } else if meta.is_file() {
            if meta.len() > MAX_MATERIAL_FILE {
                return Err(Error::rejected(
                    "installed material file exceeds the size bound",
                ));
            }
            *total = total.saturating_add(meta.len());
            if *total > MAX_MATERIAL_TOTAL {
                return Err(Error::rejected(
                    "installed material exceeds the total size bound",
                ));
            }
            let bytes = std::fs::read(&path)
                .map_err(|_| Error::unknown("installed material is unreadable"))?;
            entries.push((
                rel,
                format!(
                    "file:{:o}:{}:{}",
                    meta.mode() & 0o7777,
                    meta.len(),
                    super::profile::sha256_digest(&bytes)
                ),
            ));
        } else {
            return Err(Error::rejected(
                "installed material contains a special file",
            ));
        }
    }
    Ok(())
}

/// `/proc/<pid>/root` — the measured guest's root filesystem.
pub(crate) fn guest_root(pid: u32) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}/root"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_sorts_keys_and_escapes_like_json() {
        let value = json!({"b": [1, true, null], "a": "x\ny\"z", "c": {}});
        let bytes = canonical(&value).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "{\"a\":\"x\\ny\\\"z\",\"b\":[1,true,null],\"c\":{}}"
        );
    }

    #[test]
    fn canonical_refuses_floats_and_deep_nesting() {
        assert!(canonical(&json!({"a": 1.5})).is_err());
        let mut deep = json!(0);
        for _ in 0..20 {
            deep = json!([deep]);
        }
        assert!(canonical(&deep).is_err());
    }

    #[test]
    fn mac_material_is_fixed_ascii() {
        assert_eq!(
            mac_material(3, "ab", 9, "sha256:00"),
            b"cadence.runtime.registry/1\n3\nab\n9\nsha256:00\n".to_vec()
        );
    }
}
