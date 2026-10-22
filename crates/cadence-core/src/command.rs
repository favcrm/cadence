//! Bounded command interface for non-native hosts. Host profiles are supplied by trusted adapters.
use crate::app_compat::{self, ContractStage, HostContracts, SemVer};
use serde_json::{json, Value};
use serde_yaml::Value as Document;

pub const MAX_MANIFEST_BYTES: usize = 65_536;
pub const MAX_COMMAND_BYTES: usize = 2 * MAX_MANIFEST_BYTES + 4096;
pub const VERSION: &str = "cadence-core-command.v1";

fn refused(code: &str, message: impl std::fmt::Display) -> Value {
    json!({"ok":false,"error":{"code":code,"message":message.to_string()}})
}

/// Domain command input, not caller authentication or permission to install/execute an app.
pub fn process(input: &[u8]) -> Value {
    if input.len() > MAX_COMMAND_BYTES {
        return refused("invalid_command", "command exceeds its byte limit");
    }
    let value: Document = match serde_json::from_slice(input) {
        Ok(value) => value,
        Err(_) => return refused("invalid_command", "command must be valid UTF-8 JSON"),
    };
    let Some(command) = value.as_mapping() else {
        return refused("invalid_command", "command must be an object");
    };
    let fields = [
        "version",
        "operation",
        "manifest_json",
        "host_core_version",
        "host_contracts",
    ];
    if command.len() != fields.len()
        || command
            .keys()
            .any(|key| !key.as_str().is_some_and(|key| fields.contains(&key)))
        || value["version"].as_str() != Some(VERSION)
        || value["operation"].as_str() != Some("app.requirements.check")
    {
        return refused("invalid_command", "unsupported command shape or version");
    }
    let Some(raw) = value["manifest_json"].as_str() else {
        return refused(
            "invalid_command",
            "manifest_json must contain original JSON text",
        );
    };
    if raw.len() > MAX_MANIFEST_BYTES {
        return refused("invalid_command", "manifest exceeds its byte limit");
    }
    // Decode JSON directly into the native mapping representation: duplicate
    // keys refuse, and integer/float spelling survives without a second tree.
    let manifest: Document = match serde_json::from_str(raw) {
        Ok(Document::Mapping(object)) => Document::Mapping(object),
        _ => {
            return refused(
                "invalid_manifest",
                "manifest must be an unambiguous JSON object",
            )
        }
    };
    let Some(version) = value["host_core_version"].as_str() else {
        return refused("invalid_command", "host version is required");
    };
    let core = match SemVer::parse(version) {
        Ok(core) => core,
        Err(error) => return refused("invalid_host", error),
    };
    let Some(entries) = value["host_contracts"]
        .as_sequence()
        .filter(|entries| entries.len() <= 32)
    else {
        return refused("invalid_host", "host contracts must be a bounded array");
    };
    let mut contracts = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(object) = entry.as_mapping() else {
            return refused("invalid_host", "invalid host contract");
        };
        if object.len() != 3
            || object.keys().any(|key| {
                !key.as_str()
                    .is_some_and(|key| ["name", "majors", "stage"].contains(&key))
            })
        {
            return refused("invalid_host", "invalid host contract shape");
        }
        let Some(name) = entry["name"]
            .as_str()
            .filter(|name| !name.is_empty() && name.len() <= 32)
        else {
            return refused("invalid_host", "invalid host contract name");
        };
        if contracts.iter().any(|(existing, _, _)| *existing == name) {
            return refused("invalid_host", "duplicate host contract");
        }
        let stage = match entry["stage"].as_str() {
            Some("validator") => ContractStage::Validator,
            Some("sandboxed") => ContractStage::Sandboxed,
            _ => return refused("invalid_host", "unknown host contract stage"),
        };
        let Some(versions) = entry["majors"]
            .as_sequence()
            .filter(|versions| !versions.is_empty() && versions.len() <= 8)
        else {
            return refused("invalid_host", "invalid host contract majors");
        };
        let mut majors = Vec::with_capacity(versions.len());
        for version in versions {
            let Some(major) = version.as_u64().filter(|major| (1..=99).contains(major)) else {
                return refused(
                    "invalid_host",
                    "host contract majors must be positive integers",
                );
            };
            if majors.contains(&major) {
                return refused("invalid_host", "duplicate host contract major");
            }
            majors.push(major);
        }
        contracts.push((name, majors, stage));
    }
    let supported: Vec<_> = contracts
        .iter()
        .map(|(name, majors, stage)| (*name, majors.as_slice(), *stage))
        .collect();
    let host = HostContracts {
        core,
        supported: &supported,
    };
    let needs = match manifest.get("needs") {
        None | Some(Document::Null) => None,
        Some(Document::Mapping(mapping)) => Some(mapping),
        _ => return refused("invalid_manifest", "needs must be a mapping"),
    };
    let requirements = match app_compat::parse_requires(needs) {
        Ok(requirements) => requirements,
        Err(error) => return refused("incompatible_package", error),
    };
    match requirements.check(&host) {
        Ok(receipt) => json!({"ok":true,"version":VERSION,"receipt":receipt}),
        Err(error) => refused("incompatible_package", error),
    }
}
