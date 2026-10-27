//! Fixed, public agent guidance vendored into the binary. Each skill keeps
//! its own directory under `$HOME/.agents/skills/` and provider symlinks.
//! Company-private profiles, renderer tools and images are not installed.
//! `daemon run` and a native `cadence update` (including an already-current
//! run and rollback) refresh this guidance with the same non-force sync.
//! The bytes written are the bytes embedded in the build that is active
//! after that step, never an updater process left behind by a switch. A
//! build that predates `cadence skill sync` offers only a forced manual
//! install, so its bytes are staged out of the way and then applied here
//! with the same non-force policy: a repeat run still writes nothing.
//! Sandbox startup never writes to the shared home. File match and provider
//! links are not evidence that a running agent session has reloaded them.

use std::path::{Path, PathBuf};
use std::process::Command;

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
            "references/document-design.md",
            include_str!("../skills/company-outputs/references/document-design.md"),
        ),
        (
            "SKILL.md",
            include_str!("../skills/company-outputs/SKILL.md"),
        ),
    ],
};

const WEBSITE_BUILDER: BundledSkill = BundledSkill {
    name: "website-builder",
    files: &[
        (
            "playbook.md",
            include_str!("../skills/website-builder/playbook.md"),
        ),
        (
            "state.schema.json",
            include_str!("../skills/website-builder/state.schema.json"),
        ),
        (
            "references/site-spec.md",
            include_str!("../skills/website-builder/references/site-spec.md"),
        ),
        (
            "scripts/build.py",
            include_str!("../skills/website-builder/scripts/build.py"),
        ),
        (
            "scripts/check.py",
            include_str!("../skills/website-builder/scripts/check.py"),
        ),
        (
            "scripts/common.py",
            include_str!("../skills/website-builder/scripts/common.py"),
        ),
        (
            "scripts/next.py",
            include_str!("../skills/website-builder/scripts/next.py"),
        ),
        (
            "scripts/selftest.py",
            include_str!("../skills/website-builder/scripts/selftest.py"),
        ),
        (
            "scripts/submit.py",
            include_str!("../skills/website-builder/scripts/submit.py"),
        ),
        (
            "SKILL.md",
            include_str!("../skills/website-builder/SKILL.md"),
        ),
    ],
};

pub const SKILL_NAMES: [&str; 4] = [
    CADENCE.name,
    BUSINESS_DOCUMENTS.name,
    COMPANY_OUTPUTS.name,
    WEBSITE_BUILDER.name,
];

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
        None => Ok(&[
            CADENCE,
            BUSINESS_DOCUMENTS,
            COMPANY_OUTPUTS,
            WEBSITE_BUILDER,
        ]),
        Some("cadence") => Ok(&[CADENCE]),
        Some("business-documents") => Ok(&[BUSINESS_DOCUMENTS]),
        Some("company-outputs") => Ok(&[BUSINESS_DOCUMENTS, COMPANY_OUTPUTS]),
        Some("website-builder") => Ok(&[WEBSITE_BUILDER]),
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
        let (report, bundle_wrote) =
            write_bundle(home, &root, skill.name, skill.files.iter().copied(), force)?;
        wrote |= bundle_wrote;
        reports.push(report);
    }
    Ok(json!({"root": root, "wrote": wrote, "skills": reports}))
}

/// Write one bundle's `files` into `home` and repair its provider links.
/// Non-force unless `force`: a file whose bytes already match is left
/// untouched, so a second run writes nothing and keeps its timestamp.
/// Returns that bundle's report and whether any file was written — a
/// repaired link alone is reported in `linked` and is not a file write,
/// which is the rule `sync` has always had.
fn write_bundle<'a, I>(
    home: &Path,
    root: &Path,
    name: &str,
    files: I,
    force: bool,
) -> Result<(Value, bool)>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let dir = root.join(name);
    let mut written = Vec::new();
    for (relative, content) in files {
        let file = dir.join(relative);
        if file
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err(Error::rejected(format!(
                "refusing to write canonical skill-file symlink {}",
                file.display()
            )));
        }
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
    let mut linked = Vec::new();
    let mut skipped = Vec::new();
    for parent in LINK_PARENTS {
        let skills = home.join(parent);
        let link = skills.join(name);
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
    let wrote = !written.is_empty();
    Ok((
        json!({
            "name": name, "path": dir, "written": written,
            "linked": linked, "skipped": skipped,
        }),
        wrote,
    ))
}

/// Read-only view of one bundled skill. Without `file`, the JSON manifest
/// lists every bundled file with its byte count. With `file`, that file's
/// exact embedded text. Nothing is written; an unknown skill or file is
/// refused.
pub enum ShowOutput {
    Manifest(Value),
    File(&'static str),
}

pub fn show(name: &str, file: Option<&str>) -> Result<ShowOutput> {
    // A named skill may bring guidance dependencies (company-outputs ships
    // business-documents with it). Show the bundle the caller named, never the
    // first entry of that selection.
    let skill = selected(Some(name))?
        .iter()
        .find(|skill| skill.name == name)
        .ok_or_else(|| Error::rejected(format!("unknown bundled skill {name:?}")))?;
    match file {
        None => Ok(ShowOutput::Manifest(json!({
            "name": skill.name,
            "path": format!("skills/{}", skill.name),
            "files": skill
                .files
                .iter()
                .map(|(path, content)| json!({"path": path, "bytes": content.len()}))
                .collect::<Vec<_>>(),
        }))),
        Some(file) => skill
            .files
            .iter()
            .find(|(path, _)| *path == file)
            .map(|(_, content)| ShowOutput::File(content))
            .ok_or_else(|| {
                Error::rejected(format!("unknown file {file:?} in bundled skill {name:?}"))
            }),
    }
}

/// Inspect every expected file and provider link without changing anything.
/// `has_files` distinguishes an empty home from an existing partial install.
pub fn status(home: &Path) -> Value {
    let root = canonical_root(home);
    let mut reports = Vec::with_capacity(SKILL_NAMES.len());
    let mut installed = true;
    let mut content_match = true;
    let mut has_files = false;
    let mut repairable = false;
    let mut link_needs_attention = false;
    let mut foreign = false;
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
        if !skill_installed || !skill_match {
            repairable = true;
        }
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
        for state in links.values() {
            match state.as_str() {
                Some("ok") => {}
                Some("foreign") => foreign = true,
                _ => {
                    repairable = true;
                    link_needs_attention = true;
                }
            }
        }
        reports.push(json!({
            "name": skill.name, "path": dir, "installed": skill_installed,
            "content_match": skill_match, "files": files, "links": links,
        }));
    }
    let current = installed && content_match && !repairable && !foreign;
    let guidance = if current {
        "current"
    } else {
        "needs-attention"
    };
    let repair = if repairable {
        Value::String("cadence skill install".to_string())
    } else {
        Value::Null
    };
    let attention = if foreign {
        Value::String(
            "a foreign provider entry was left alone; remove it by hand if this install should own the link"
                .to_string(),
        )
    } else {
        Value::Null
    };
    json!({
        "root": root, "installed": installed, "has_files": has_files,
        "content_match": content_match, "skills": reports,
        "home": home,
        "build": crate::overview::BUILD_COMMIT,
        "guidance": guidance,
        "repair": repair,
        "attention": attention,
        "file_match": if content_match { "current" } else { "needs-attention" },
        "provider_discovery": if link_needs_attention || foreign {
            "needs-attention"
        } else {
            "current"
        },
        "live_session": "not-checked",
        "live_session_note": "File match and provider links do not show that an agent loaded this guidance. An existing session keeps what it already read; start a fresh session or reread the skill at a task boundary.",
    })
}

/// Which build's embedded guidance an update may write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivatedGuidance {
    /// No switch happened: this process is the active build.
    CurrentProcess,
    /// Execute `releases/<sha>/cadence` so the write uses that build's bytes.
    Release(String),
    /// The active build is not identified. Do not write.
    Unavailable,
}

/// `health` and `restart` are the update report's JSON fields.
/// A rollback prefers `rolled_back_to` over the failed candidate's `build`.
pub fn activated_guidance(rolled_back: bool, health: &Value, restart: &Value) -> ActivatedGuidance {
    if !rolled_back && health.is_null() && restart.is_null() {
        return ActivatedGuidance::CurrentProcess;
    }
    let sha = if rolled_back {
        health["rolled_back_to"]
            .as_str()
            .or_else(|| health["build"].as_str())
            .or_else(|| restart["sha"].as_str())
    } else {
        health["build"].as_str().or_else(|| restart["sha"].as_str())
    };
    match sha.map(str::trim).filter(|sha| !sha.is_empty()) {
        Some(sha) => ActivatedGuidance::Release(sha.to_string()),
        None => ActivatedGuidance::Unavailable,
    }
}

/// `cwd/skills/<bundled-name>` is an unmanaged snapshot. This only
/// reports paths that are already there; it does not search, read
/// bodies, or change them.
pub fn unmanaged_snapshots(cwd: &Path) -> Vec<Value> {
    let mut found = Vec::new();
    for name in SKILL_NAMES {
        let path = cwd.join("skills").join(name);
        if path.symlink_metadata().is_ok() {
            found.push(json!({
                "name": name,
                "path": path,
                "role": "unmanaged",
                "active": false,
                "note": "workspace snapshot only; not an agent install and not modified",
            }));
        }
    }
    found
}

/// Synchronize guidance for the activated build. A build that has
/// `skill sync` runs it there — that build's own non-force sync over its
/// own bytes.
///
/// A build from before `skill sync` existed offers only its forced manual
/// install, which rewrites every file it ships. It is asked for its bytes
/// in a private staging home, and this process then applies those bytes
/// here with the non-force policy every other path uses: only a missing or
/// differing file is written, so a second unchanged run writes nothing.
/// The bytes are always the activated build's, never this process's, and
/// nothing the child writes in the staging home reaches this home.
pub fn invoke_activated_sync(binary: &Path, home: &Path, state_dir: &Path) -> Value {
    if !binary.is_file() {
        return json!({
            "guidance": "not-checked",
            "wrote": false,
            "reason": format!("activated binary {} is not a file; guidance was not written", binary.display()),
            "live_session": "not-checked",
        });
    }
    if let Some(report) = run_skill_json(binary, home, state_dir, &["skill", "sync"]) {
        return normalize_sync_report(report, binary);
    }
    legacy_activated_sync(binary, home)
}

/// The compatibility path for an activated build that predates
/// `skill sync`. The child runs against a staging home and a staging state
/// dir, so nothing it does can reach this home or this runtime state.
fn legacy_activated_sync(binary: &Path, home: &Path) -> Value {
    let not_checked = |reason: String| {
        json!({
            "guidance": "not-checked",
            "wrote": false,
            "reason": reason,
            "repair": "cadence skill install",
            "live_session": "not-checked",
            "activated_binary": binary,
        })
    };
    let staged = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(e) => return not_checked(format!("staging home was not created: {e}")),
    };
    let staged_home = staged.path().join("home");
    let staged_state = staged.path().join("state");
    if let Err(e) =
        std::fs::create_dir_all(&staged_home).and_then(|()| std::fs::create_dir_all(&staged_state))
    {
        return not_checked(format!("staging home was not created: {e}"));
    }
    let Some(sync) = run_skill_json(binary, &staged_home, &staged_state, &["skill", "install"])
    else {
        return not_checked(
            "activated binary did not report guidance; this process did not write it".into(),
        );
    };
    let applied = apply_staged_bundle(&staged_home, home);
    let foreign = applied["skills"].as_array().is_some_and(|skills| {
        skills.iter().any(|skill| {
            skill["skipped"]
                .as_array()
                .is_some_and(|skipped| !skipped.is_empty())
        })
    });
    let error = applied["error"].clone();
    let clean = !foreign && error.is_null();
    json!({
        "guidance": if clean { "current" } else { "needs-attention" },
        "wrote": applied["wrote"],
        "repair": if clean { Value::Null } else { json!("cadence skill install") },
        "attention": if foreign {
            json!("a foreign provider entry was left alone; remove it by hand if this install should own the link")
        } else {
            Value::Null
        },
        "error": error,
        "applied": applied,
        "sync": sync,
        "source_command": "skill install",
        "staged": true,
        "live_session": "not-checked",
        "live_session_note": "File match and provider links do not show that an agent loaded this guidance. An existing session keeps what it already read; start a fresh session or reread the skill at a task boundary.",
        "activated_binary": binary,
    })
}

fn run_skill_json(binary: &Path, home: &Path, state_dir: &Path, args: &[&str]) -> Option<Value> {
    let mut command = Command::new(binary);
    command
        .args(["--state-dir"])
        .arg(state_dir)
        .args(args)
        .env("HOME", home)
        .env("CADENCE_JSON", "compact")
        .env_remove("CADENCE_ORG")
        .env_remove("CADENCE_ALIAS");
    let output = crate::reaper::output(&mut command).ok()?;
    if !output.status.success() {
        return None;
    }
    parse_json_stdout(&String::from_utf8_lossy(&output.stdout))
}

fn parse_json_stdout(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    serde_json::from_str(trimmed).ok().or_else(|| {
        trimmed
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str(line).ok())
    })
}

fn normalize_sync_report(mut parsed: Value, binary: &Path) -> Value {
    if parsed.get("guidance").is_none() {
        if let Some(status) = parsed.get("status").cloned() {
            if parsed.get("wrote").is_none() {
                parsed["wrote"] = status.get("wrote").cloned().unwrap_or(Value::Bool(false));
            }
            parsed["guidance"] = status
                .get("guidance")
                .cloned()
                .unwrap_or(Value::String("not-checked".into()));
            parsed["repair"] = status.get("repair").cloned().unwrap_or(Value::Null);
            parsed["live_session"] = json!("not-checked");
            parsed["live_session_note"] = status
                .get("live_session_note")
                .cloned()
                .unwrap_or(Value::Null);
        }
    }
    parsed["activated_binary"] = json!(binary);
    parsed
}

/// Apply a staged activated build's bytes to `home` with the non-force
/// policy: only a missing or differing file is written, and only a missing
/// or ours-wrong provider link is repaired. The managed path list is this
/// build's, so no path the activated build never shipped is invented here,
/// and a file this build no longer ships is never deleted. A write or link
/// failure is reported, not hidden, and stops the remaining bundles.
fn apply_staged_bundle(staged_home: &Path, home: &Path) -> Value {
    let root = canonical_root(home);
    let staged_root = canonical_root(staged_home);
    let mut reports = Vec::new();
    let mut wrote = false;
    let mut error = Value::Null;
    for skill in selected(None).expect("the complete bundled set is valid") {
        let staged_dir = staged_root.join(skill.name);
        let files: Vec<(&str, String)> = skill
            .files
            .iter()
            .filter_map(|&(relative, _)| {
                std::fs::read_to_string(staged_dir.join(relative))
                    .ok()
                    .map(|content| (relative, content))
            })
            .collect();
        if files.is_empty() {
            continue;
        }
        match write_bundle(
            home,
            &root,
            skill.name,
            files
                .iter()
                .map(|(relative, content)| (*relative, content.as_str())),
            false,
        ) {
            Ok((report, bundle_wrote)) => {
                wrote |= bundle_wrote;
                reports.push(report);
            }
            Err(e) => {
                error = json!(e.to_string());
                break;
            }
        }
    }
    json!({"root": root, "wrote": wrote, "skills": reports, "error": error})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_current_update_uses_this_process_and_rollback_uses_the_restored_sha() {
        assert_eq!(
            activated_guidance(false, &Value::Null, &Value::Null),
            ActivatedGuidance::CurrentProcess
        );
        let switched = json!({"ok": true, "build": "new-sha"});
        assert_eq!(
            activated_guidance(false, &switched, &json!({"sha": "new-sha"})),
            ActivatedGuidance::Release("new-sha".into())
        );
        let auto = json!({"ok": false, "build": "failed-sha", "rolled_back_to": "restored-sha"});
        assert_eq!(
            activated_guidance(true, &auto, &Value::Null),
            ActivatedGuidance::Release("restored-sha".into())
        );
        let explicit = json!({"ok": true, "build": "restored-sha"});
        assert_eq!(
            activated_guidance(true, &explicit, &json!({"sha": "restored-sha"})),
            ActivatedGuidance::Release("restored-sha".into())
        );
        assert_eq!(
            activated_guidance(true, &json!({}), &json!({})),
            ActivatedGuidance::Unavailable
        );
    }

    #[test]
    fn unmanaged_snapshot_is_reported_and_not_modified() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("skills/company-outputs");
        std::fs::create_dir_all(snapshot.join("references")).unwrap();
        let body = snapshot.join("SKILL.md");
        std::fs::write(&body, "old snapshot\n").unwrap();
        let found = unmanaged_snapshots(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["active"], false);
        assert_eq!(std::fs::read(&body).unwrap(), b"old snapshot\n");
        assert!(unmanaged_snapshots(&dir.path().join("empty")).is_empty());
    }

    #[test]
    fn status_separates_file_match_links_and_live_session() {
        let dir = tempfile::tempdir().unwrap();
        let empty = status(dir.path());
        assert_eq!(empty["guidance"], "needs-attention");
        assert_eq!(empty["repair"], "cadence skill install");
        assert_eq!(empty["live_session"], "not-checked");
        assert_eq!(empty["file_match"], "needs-attention");
        assert_eq!(empty["provider_discovery"], "needs-attention");
        assert!(empty["content_match"].is_boolean());
        assert!(empty["skills"].is_array());

        sync(dir.path(), false, None).unwrap();
        let current = status(dir.path());
        assert_eq!(current["file_match"], "current");
        assert_eq!(current["provider_discovery"], "current");
        assert_eq!(current["guidance"], "current");
        assert!(current["repair"].is_null());

        let stale = canonical_root(dir.path()).join("cadence/SKILL.md");
        std::fs::write(&stale, "stale guidance\n").unwrap();
        let files_only = status(dir.path());
        assert_eq!(files_only["file_match"], "needs-attention");
        assert_eq!(files_only["provider_discovery"], "current");
        assert_eq!(files_only["repair"], "cadence skill install");

        sync(dir.path(), false, None).unwrap();
        let link = dir.path().join(".claude/skills/cadence");
        std::fs::remove_file(&link).unwrap();
        let links_only = status(dir.path());
        assert_eq!(links_only["file_match"], "current");
        assert_eq!(links_only["content_match"], true);
        assert_eq!(links_only["provider_discovery"], "needs-attention");
        assert_eq!(links_only["repair"], "cadence skill install");

        sync(dir.path(), false, None).unwrap();
        std::fs::remove_file(&link).unwrap();
        std::fs::create_dir(&link).unwrap();
        let foreign_only = status(dir.path());
        assert_eq!(foreign_only["file_match"], "current");
        assert_eq!(foreign_only["provider_discovery"], "needs-attention");
        assert!(foreign_only["repair"].is_null());
        assert!(foreign_only["attention"]
            .as_str()
            .unwrap()
            .contains("foreign"));
    }

    #[test]
    fn website_builder_selects_only_its_own_bundle() {
        let only = selected(Some("website-builder")).unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].name, "website-builder");
        assert!(SKILL_NAMES.contains(&"website-builder"));
    }

    #[test]
    fn website_builder_install_writes_all_ten_files_then_second_sync_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let first = sync(dir.path(), true, Some("website-builder")).unwrap();
        assert_eq!(first["wrote"], true);
        let root = canonical_root(dir.path()).join("website-builder");
        for (path, _) in WEBSITE_BUILDER.files {
            assert!(root.join(path).is_file(), "missing {path}");
        }
        assert_eq!(WEBSITE_BUILDER.files.len(), 10);

        let second = sync(dir.path(), false, Some("website-builder")).unwrap();
        assert_eq!(second["wrote"], false);
    }

    #[test]
    fn skill_show_lists_manifest_with_byte_counts_and_refuses_unknown_file() {
        let manifest = match show("website-builder", None).unwrap() {
            ShowOutput::Manifest(value) => value,
            ShowOutput::File(_) => panic!("manifest expected"),
        };
        assert_eq!(manifest["name"], "website-builder");
        let files = manifest["files"].as_array().unwrap();
        assert_eq!(files.len(), 10);
        for row in files {
            assert!(row["bytes"].as_u64().unwrap() > 0);
        }
        match show("website-builder", Some("scripts/next.py")).unwrap() {
            ShowOutput::File(text) => assert_eq!(
                text,
                include_str!("../skills/website-builder/scripts/next.py")
            ),
            ShowOutput::Manifest(_) => panic!("file expected"),
        }
        let refused = show("website-builder", Some("../../etc/passwd"));
        assert!(refused.is_err());
        assert!(show("no-such-skill", None).is_err());
    }

    /// company-outputs installs business-documents with it; `show` must name
    /// the bundle the caller asked for, not the first entry of that selection.
    #[test]
    fn skill_show_names_the_requested_bundle_not_its_dependency() {
        let paths = |value: &Value| -> Vec<String> {
            value["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["path"].as_str().unwrap().to_string())
                .collect()
        };
        let company = match show("company-outputs", None).unwrap() {
            ShowOutput::Manifest(value) => value,
            ShowOutput::File(_) => panic!("manifest expected"),
        };
        assert_eq!(company["name"], "company-outputs");
        assert!(paths(&company).contains(&"references/proposal.md".to_string()));
        let business = match show("business-documents", None).unwrap() {
            ShowOutput::Manifest(value) => value,
            ShowOutput::File(_) => panic!("manifest expected"),
        };
        assert_eq!(business["name"], "business-documents");
        assert!(!paths(&business).contains(&"references/proposal.md".to_string()));
    }
}
