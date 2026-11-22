//! App skill-package manifest validation, digesting, and reference checks.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

use super::{
    skill_member_ok, SKILL_DIR, SKILL_ENTRY, SKILL_FILES_MAX, SKILL_MANIFEST, SKILL_MANIFEST_BYTES,
    SKILL_REFERENCES_EXTS, SKILL_ROOT_EXTS, SKILL_SCRIPTS_EXTS,
};

mod package;

pub use package::{skill_package, skill_packages};

/// The `skill.json` a package must carry. Closed decode: an unknown field
/// refuses, so a package cannot smuggle a contract this host does not read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillManifest {
    pub schema: u32,
    pub name: String,
    /// Every member of the package except `skill.json` itself, in declared
    /// order. The set must equal the set actually present — both directions
    /// refuse, naming the path.
    pub files: Vec<String>,
}

/// One declared member, frozen: the exact bytes the host read, and their
/// digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillFile {
    pub path: String,
    pub sha256: String,
    pub text: String,
}

/// A skill package frozen at run creation: `{name, manifest,
/// manifest_sha256, digest, files}`. This value travels in the run snapshot
/// and in the dispatch kickoff and nowhere else — the host materializes no
/// copy on disk, so a later bundle update cannot change what a run was given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackage {
    pub name: String,
    pub manifest: SkillManifest,
    pub manifest_sha256: String,
    pub digest: String,
    pub files: Vec<SkillFile>,
}

/// The one skill manifest validator: install validation and run creation both
/// reach a package through [`skill_package`], which is the only caller of
/// this, so the two can never disagree about what a complete package is.
///
/// `tag` is the package's directory name. The manifest's own `name` must be
/// that tag — a package cannot declare itself to be another one.
pub fn parse_skill_manifest(text: &str, tag: &str) -> Result<SkillManifest> {
    if !crate::issue::model::valid_tag(tag) {
        return Err(Error::rejected(format!(
            "skill package '{tag}' — a package name is [a-z0-9-], ≤32 bytes"
        )));
    }
    if text.len() as u64 > SKILL_MANIFEST_BYTES {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} is {} bytes — a skill manifest is at \
             most {SKILL_MANIFEST_BYTES}",
            text.len()
        )));
    }
    let manifest: SkillManifest = serde_json::from_str(text).map_err(|e| {
        Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST}: {e} — the manifest is \
             {{\"schema\":1,\"name\":\"{tag}\",\"files\":[\"{SKILL_ENTRY}\"]}}"
        ))
    })?;
    if manifest.schema != 1 {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST}: schema {} — this host reads schema 1",
            manifest.schema
        )));
    }
    if manifest.name != tag {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} declares name '{}' — it must be its \
             own directory name '{tag}'",
            manifest.name
        )));
    }
    if manifest.files.is_empty() {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} declares no files — it must declare \
             {SKILL_ENTRY} and every member the package carries"
        )));
    }
    if manifest.files.len() > SKILL_FILES_MAX {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag} declares {} files — a package declares at most \
             {SKILL_FILES_MAX}",
            manifest.files.len()
        )));
    }
    for (i, member) in manifest.files.iter().enumerate() {
        if member == SKILL_MANIFEST {
            return Err(Error::rejected(format!(
                "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} lists '{SKILL_MANIFEST}' — the \
                 manifest is never one of its own declared files"
            )));
        }
        if !skill_member_ok(member) {
            return Err(Error::rejected(format!(
                "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} declares '{member}' — a package \
                 member is a flat root <stem>.<{}>, or scripts/<stem>.<{}>, or \
                 references/<stem>.<{}>",
                SKILL_ROOT_EXTS.join("|"),
                SKILL_SCRIPTS_EXTS.join("|"),
                SKILL_REFERENCES_EXTS.join("|"),
            )));
        }
        if manifest.files[..i].contains(member) {
            return Err(Error::rejected(format!(
                "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} declares '{member}' twice"
            )));
        }
    }
    if !manifest.files.iter().any(|member| member == SKILL_ENTRY) {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} must declare {SKILL_ENTRY} — the \
             instructions an agent reads first"
        )));
    }
    Ok(manifest)
}

/// The digest over a frozen package: the name, the manifest bytes and every
/// declared member's path and digest, length-prefixed in declared order.
fn skill_digest(name: &str, manifest_sha256: &str, files: &[SkillFile]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"cadence-app-skill-v1\n");
    for part in [name, manifest_sha256] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    for file in files {
        for part in [file.path.as_str(), file.sha256.as_str()] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
    }
    format!("sha256:{:x}", digest.finalize())
}
