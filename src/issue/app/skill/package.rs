//! Skill-package bundle resolution and undeclared-reference validation.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, Result};

use super::super::{
    sha256_hex, skill_member_ok, SKILL_ALL_EXTS, SKILL_DIR, SKILL_FILE_BYTES, SKILL_MANIFEST,
    SKILL_PACKAGE_BYTES, SKILL_REFERENCES_EXTS, SKILL_ROOT_EXTS, SKILL_SCRIPTS_EXTS,
};
use super::{parse_skill_manifest, skill_digest, SkillFile, SkillPackage};

/// Tokens in `text` that could name package material. A token is a run of
/// ASCII alphanumerics plus `.`, `_`, `/` and `-`; everything else is a
/// separator. Every token is returned: dropping a long one would be a
/// reference this guard silently ignores, and the files are bounded anyway.
fn reference_tokens(text: &str) -> impl Iterator<Item = &str> + '_ {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-')))
        .filter(|token| !token.is_empty())
}

/// The package-relative path a token names, or `None` when the token names
/// nothing this contract tracks.
///
/// Every `.`-led spelling is the same relative path: `./x`, `.//x` and
/// `.//./x` all name `x`, and `.`/empty segments carry no meaning. Only a token
/// that starts with a slash and was never `.`-led is an absolute path, which is
/// somebody else's file. `..` would escape the package and refuses.
fn referenced_member(token: &str, tag: &str) -> Option<Reference> {
    // A sentence-ending period is a separator, not part of a member name:
    // `run ./scripts/build.py.` names `scripts/build.py`.
    let token = token.trim_end_matches('.');
    if token.is_empty() {
        return None;
    }
    let absolute = token.starts_with('/') && !token.starts_with('.');
    let mut escaped = false;
    let mut segments: Vec<&str> = Vec::new();
    for segment in token.split('/') {
        match segment {
            "" | "." => continue,
            // `..` cannot be resolved against the package, so a token that
            // uses it and still names package material refuses below rather
            // than being dropped as if it named nothing.
            ".." => escaped = true,
            other => segments.push(other),
        }
    }
    if segments.is_empty() || absolute {
        return None;
    }
    let path = segments.join("/");
    if let Some(rest) = path.strip_prefix(&format!("{SKILL_DIR}/")) {
        let (owner, member) = rest.split_once('/')?;
        return Some(Reference {
            owner: owner.to_string(),
            member: member.to_string(),
            escaped,
        });
    }
    for directory in ["scripts", "references"] {
        if path.starts_with(&format!("{directory}/")) {
            return Some(Reference {
                owner: tag.to_string(),
                member: path,
                escaped,
            });
        }
    }
    if path.contains('/') {
        return None;
    }
    let ext = path.rsplit_once('.').map(|(_, ext)| ext)?;
    if !SKILL_ALL_EXTS.contains(&ext) {
        return None;
    }
    Some(Reference {
        owner: tag.to_string(),
        member: path,
        escaped,
    })
}

/// One token that names package material: the package it names (`skills/` may
/// name another package, which refuses) and the member path inside it.
/// `escaped` records a `..` segment, which cannot be resolved inside the
/// package and therefore refuses instead of being ignored.
struct Reference {
    owner: String,
    member: String,
    escaped: bool,
}

/// CAD-1867: the declared-inventory check. Every declared text file is
/// scanned for tokens that name package material, and a token naming a member
/// the manifest does not declare refuses.
///
/// This is a declared-inventory contract, NOT import analysis: it reads names,
/// never evaluates them, so a path assembled at run time (`os.path.join`,
/// `importlib`, a variable) is invisible to it. It fails closed on the names it
/// can see — including a bare `name.ext` with an allowed extension, which is
/// read as a package member — because a silently ignored reference is a member
/// an agent would look for and not find.
fn undeclared_references(
    files: &[SkillFile],
    declared: &BTreeSet<String>,
    tag: &str,
) -> Vec<String> {
    let mut refusals: Vec<String> = Vec::new();
    for file in files {
        for token in reference_tokens(&file.text) {
            let Some(reference) = referenced_member(token, tag) else {
                continue;
            };
            let refusal = if reference.escaped {
                format!(
                    "{SKILL_DIR}/{tag}/{path}: '{token}' reaches package material through '..' — \
                     name the member path inside the package",
                    path = file.path
                )
            } else if reference.owner != tag {
                format!(
                    "{SKILL_DIR}/{tag}/{path}: '{token}' names skill package '{owner}' — a \
                     package references only its own declared members",
                    path = file.path,
                    owner = reference.owner
                )
            } else if declared.contains(&reference.member) {
                continue;
            } else {
                format!(
                    "{SKILL_DIR}/{tag}/{path}: '{token}' is not declared in \
                     {SKILL_DIR}/{tag}/{SKILL_MANIFEST} — declare '{member}' in its files",
                    path = file.path,
                    member = reference.member
                )
            };
            if !refusals.contains(&refusal) {
                refusals.push(refusal);
            }
        }
    }
    refusals
}

/// Resolve ONE skill package out of a bundle's file map — `rel-path → text`.
///
/// This is the only package validator: install validation runs it over every
/// package a bundle carries and over every tag a workflow names, and run
/// creation runs it over the tag it freezes, so the two cannot disagree. The
/// map is the bundle's own bytes (the installed file map at run creation);
/// nothing here touches the filesystem.
pub fn skill_package(files: &BTreeMap<String, String>, tag: &str) -> Result<SkillPackage> {
    if !crate::issue::model::valid_tag(tag) {
        return Err(Error::rejected(format!(
            "skill '{tag}' — a skill name is [a-z0-9-], ≤32 bytes"
        )));
    }
    let prefix = format!("{SKILL_DIR}/{tag}/");
    let manifest_path = format!("{prefix}{SKILL_MANIFEST}");
    let manifest_text = files.get(&manifest_path).ok_or_else(|| {
        Error::rejected(format!(
            "no skill package '{tag}' in this bundle — it needs {manifest_path}"
        ))
    })?;
    let manifest = parse_skill_manifest(manifest_text, tag)?;
    // What the bundle actually carries under this package.
    let mut present: BTreeSet<String> = BTreeSet::new();
    for rel in files.keys().filter_map(|rel| rel.strip_prefix(&prefix)) {
        if !skill_member_ok(rel) {
            return Err(Error::rejected(format!(
                "{prefix}{rel} — a package member is a flat root <stem>.<{}>, or \
                 scripts/<stem>.<{}>, or references/<stem>.<{}>; nothing deeper, no \
                 dotfiles",
                SKILL_ROOT_EXTS.join("|"),
                SKILL_SCRIPTS_EXTS.join("|"),
                SKILL_REFERENCES_EXTS.join("|"),
            )));
        }
        present.insert(rel.to_string());
    }
    // The manifest was loaded from this same map before the scan. It is a
    // root text leaf, so the scan above keeps it in `present`; the declared
    // set deliberately excludes it.
    let declared: BTreeSet<String> = manifest.files.iter().cloned().collect();
    for member in &declared {
        if !present.contains(member) {
            return Err(Error::rejected(format!(
                "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} declares '{member}' but the bundle \
                 does not carry {prefix}{member} — a package declares exactly what it \
                 ships"
            )));
        }
    }
    for member in &present {
        if member != SKILL_MANIFEST && !declared.contains(member) {
            return Err(Error::rejected(format!(
                "{prefix}{member} is not declared in {SKILL_DIR}/{tag}/{SKILL_MANIFEST} \
                 — declare it in the manifest's files, or remove it"
            )));
        }
    }
    let mut total = manifest_text.len() as u64;
    let mut frozen: Vec<SkillFile> = Vec::with_capacity(manifest.files.len());
    for member in &manifest.files {
        let text = files.get(&format!("{prefix}{member}")).ok_or_else(|| {
            Error::internal(format!(
                "{prefix}{member} vanished after the presence check"
            ))
        })?;
        if text.len() as u64 > SKILL_FILE_BYTES {
            return Err(Error::rejected(format!(
                "{prefix}{member} is {} bytes — a skill member is at most {SKILL_FILE_BYTES}",
                text.len()
            )));
        }
        total += text.len() as u64;
        frozen.push(SkillFile {
            path: member.clone(),
            sha256: format!("sha256:{}", sha256_hex(text.as_bytes())),
            text: text.clone(),
        });
    }
    if total > SKILL_PACKAGE_BYTES {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag} is {total} bytes — a skill package is at most \
             {SKILL_PACKAGE_BYTES} including its manifest"
        )));
    }
    let refusals = undeclared_references(&frozen, &declared, tag);
    if !refusals.is_empty() {
        return Err(Error::rejected(refusals.join("; ")));
    }
    let manifest_sha256 = format!("sha256:{}", sha256_hex(manifest_text.as_bytes()));
    Ok(SkillPackage {
        name: tag.to_string(),
        digest: skill_digest(tag, &manifest_sha256, &frozen),
        manifest,
        manifest_sha256,
        files: frozen,
    })
}

/// Every skill package a bundle carries, by tag.
pub fn skill_packages(files: &BTreeMap<String, String>) -> Result<Vec<SkillPackage>> {
    let mut tags: Vec<&str> = Vec::new();
    for rel in files.keys() {
        let Some(rest) = rel.strip_prefix(&format!("{SKILL_DIR}/")) else {
            continue;
        };
        let Some((tag, _)) = rest.split_once('/') else {
            return Err(Error::rejected(format!(
                "{SKILL_DIR}/{rest} — a skill package is a {SKILL_DIR}/<tag>/ directory"
            )));
        };
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags.into_iter()
        .map(|tag| skill_package(files, tag))
        .collect()
}
