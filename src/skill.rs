//! Fixed, public agent guidance vendored into the binary. Each skill keeps
//! its own directory under `$HOME/.agents/skills/` and provider symlinks.
//! Company-private profiles, renderer tools and images are not installed.
//! `daemon run` refreshes this guidance using the same sync path; sandbox
//! startup never writes to the shared home.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};

/// The vendored skill document — single source of truth.
pub const SKILL_MD: &str = include_str!("../skills/cadence/SKILL.md");

struct BundledSkill {
    name: &'static str,
    files: &'static [(&'static str, &'static str)],
}

const CADENCE: BundledSkill = BundledSkill {
    name: "cadence",
    files: &[("SKILL.md", SKILL_MD)],
};
const BUSINESS_DOCUMENTS: BundledSkill = BundledSkill {
    name: "business-documents",
    files: &[
        (
            "OPERATOR.md",
            include_str!("../skills/business-documents/OPERATOR.md"),
        ),
        (
            "SKILL.md",
            include_str!("../skills/business-documents/SKILL.md"),
        ),
    ],
};
const COMPANY_OUTPUTS: BundledSkill = BundledSkill {
    name: "company-outputs",
    // Publish the entry point after its references, not before them.
    files: &[
        (
            "references/workflow.md",
            include_str!("../skills/company-outputs/references/workflow.md"),
        ),
        (
            "references/proposal.md",
            include_str!("../skills/company-outputs/references/proposal.md"),
        ),
        (
            "references/quotation.md",
            include_str!("../skills/company-outputs/references/quotation.md"),
        ),
        (
            "references/cadence.md",
            include_str!("../skills/company-outputs/references/cadence.md"),
        ),
        (
            "SKILL.md",
            include_str!("../skills/company-outputs/SKILL.md"),
        ),
    ],
};

pub const SKILL_NAMES: [&str; 3] = [CADENCE.name, BUSINESS_DOCUMENTS.name, COMPANY_OUTPUTS.name];

/// Provider skill directories that receive links to each selected bundle.
const LINK_PARENTS: [&str; 3] = [".claude/skills", ".cursor/skills", ".copilot/skills"];

fn canonical_root(home: &Path) -> PathBuf {
    home.join(".agents").join("skills")
}

/// Real provider dirs/files are never touched. Only missing or symlink
/// entries are managed.
fn link_state(link: &Path, target: &Path) -> &'static str {
    match std::fs::symlink_metadata(link) {
        Err(_) => "missing",
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(link) {
            Ok(t) if t == target => "ok",
            _ => "wrong-target",
        },
        Ok(_) => "foreign",
    }
}

fn selected(target: Option<&str>) -> Result<&'static [BundledSkill]> {
    match target {
        None => Ok(&[CADENCE, BUSINESS_DOCUMENTS, COMPANY_OUTPUTS]),
        Some("cadence") => Ok(&[CADENCE]),
        Some("business-documents") => Ok(&[BUSINESS_DOCUMENTS]),
        Some("company-outputs") => Ok(&[BUSINESS_DOCUMENTS, COMPANY_OUTPUTS]),
        Some(name) => Err(Error::rejected(format!(
            "unknown bundled skill {name:?}; expected {}",
            SKILL_NAMES.join(", ")
        ))),
    }
}

/// Refresh selected public guidance, including company-outputs' guidance
/// dependency. Validate the fixed name before any filesystem writes.
pub fn sync(home: &Path, force: bool, target: Option<&str>) -> Result<Value> {
    let selected = selected(target)?;
    let root = canonical_root(home);
    let mut reports = Vec::with_capacity(selected.len());
    let mut wrote = false;
    for skill in selected {
        let dir = root.join(skill.name);
        let mut written = Vec::new();
        for &(relative, content) in skill.files {
            let file = dir.join(relative);
            let stale = force
                || std::fs::read(&file)
                    .map(|bytes| bytes != content.as_bytes())
                    .unwrap_or(true);
            if stale {
                std::fs::create_dir_all(file.parent().expect("bundled file has a parent"))
                    .map_err(|e| Error::provider(format!("create {}: {e}", dir.display())))?;
                std::fs::write(&file, content)
                    .map_err(|e| Error::provider(format!("write {}: {e}", file.display())))?;
                written.push(file);
            }
        }
        wrote |= !written.is_empty();
        let mut linked = Vec::new();
        let mut skipped = Vec::new();
        for parent in LINK_PARENTS {
            let skills = home.join(parent);
            let link = skills.join(skill.name);
            match link_state(&link, &dir) {
                "ok" => continue,
                "foreign" => {
                    skipped.push(json!({
                        "path": link, "reason": "real entry exists — left alone"}));
                    continue;
                }
                _ => {}
            }
            std::fs::create_dir_all(&skills)
                .map_err(|e| Error::provider(format!("create {}: {e}", skills.display())))?;
            if link.symlink_metadata().is_ok() {
                std::fs::remove_file(&link)
                    .map_err(|e| Error::provider(format!("replace {}: {e}", link.display())))?;
            }
            std::os::unix::fs::symlink(&dir, &link)
                .map_err(|e| Error::provider(format!("link {}: {e}", link.display())))?;
            linked.push(link);
        }
        reports.push(json!({
            "name": skill.name, "path": dir, "written": written,
            "linked": linked, "skipped": skipped,
        }));
    }
    Ok(json!({"root": root, "wrote": wrote, "skills": reports}))
}

/// Inspect every expected file and provider link without changing anything.
/// `has_files` distinguishes an empty home from an existing partial install.
pub fn status(home: &Path) -> Value {
    let root = canonical_root(home);
    let mut reports = Vec::with_capacity(SKILL_NAMES.len());
    let mut installed = true;
    let mut content_match = true;
    let mut has_files = false;
    for skill in selected(None).expect("the complete bundled set is valid") {
        let dir = root.join(skill.name);
        let mut files = Vec::with_capacity(skill.files.len());
        let mut skill_installed = true;
        let mut skill_match = true;
        for &(relative, content) in skill.files {
            let file = dir.join(relative);
            let exists = file.is_file();
            let matches = std::fs::read(&file)
                .map(|bytes| bytes == content.as_bytes())
                .unwrap_or(false);
            has_files |= exists;
            skill_installed &= exists;
            skill_match &= matches;
            files.push(json!({
                "path": file, "installed": exists, "content_match": matches,
            }));
        }
        installed &= skill_installed;
        content_match &= skill_match;
        let links: serde_json::Map<String, Value> = LINK_PARENTS
            .iter()
            .map(|parent| {
                let name = parent
                    .split('/')
                    .next()
                    .unwrap_or(parent)
                    .trim_start_matches('.');
                (
                    name.to_string(),
                    json!(link_state(&home.join(parent).join(skill.name), &dir)),
                )
            })
            .collect();
        reports.push(json!({
            "name": skill.name, "path": dir, "installed": skill_installed,
            "content_match": skill_match, "files": files, "links": links,
        }));
    }
    json!({
        "root": root, "installed": installed, "has_files": has_files,
        "content_match": content_match, "skills": reports,
    })
}
