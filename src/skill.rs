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

use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};

mod sync;

pub use sync::{
    activated_guidance, invoke_activated_sync, show, status, sync, unmanaged_snapshots,
    ActivatedGuidance, ShowOutput,
};

/// The vendored skill document — single source of truth.
pub const SKILL_MD: &str = include_str!("../skills/cadence/SKILL.md");

pub(super) struct BundledSkill {
    pub(super) name: &'static str,
    pub(super) files: &'static [(&'static str, &'static str)],
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

const INCIDENT_RESPONSE: BundledSkill = BundledSkill {
    name: "incident-response",
    // Publish the entry point after its references, not before them.
    files: &[
        (
            "references/postmortem-template.html",
            include_str!("../skills/incident-response/references/postmortem-template.html"),
        ),
        (
            "SKILL.md",
            include_str!("../skills/incident-response/SKILL.md"),
        ),
    ],
};

const DELIVERABLE_QA: BundledSkill = BundledSkill {
    name: "deliverable-qa",
    files: &[(
        "SKILL.md",
        include_str!("../skills/deliverable-qa/SKILL.md"),
    )],
};

const CADENCE_APP_DEV: BundledSkill = BundledSkill {
    name: "cadence-app-dev",
    files: &[
        (
            "references/contracts-and-storage.md",
            include_str!("../skills/cadence-app-dev/references/contracts-and-storage.md"),
        ),
        (
            "references/bridge-and-ui.md",
            include_str!("../skills/cadence-app-dev/references/bridge-and-ui.md"),
        ),
        (
            "references/release-and-catalog.md",
            include_str!("../skills/cadence-app-dev/references/release-and-catalog.md"),
        ),
        (
            "SKILL.md",
            include_str!("../skills/cadence-app-dev/SKILL.md"),
        ),
    ],
};

const ASSISTANT_MEMORY: BundledSkill = BundledSkill {
    name: "assistant-memory",
    files: &[(
        "SKILL.md",
        include_str!("../skills/assistant-memory/SKILL.md"),
    )],
};

pub const REQUIRED_SKILL: &str = CADENCE.name;

pub const SKILL_NAMES: [&str; 8] = [
    CADENCE.name,
    BUSINESS_DOCUMENTS.name,
    COMPANY_OUTPUTS.name,
    WEBSITE_BUILDER.name,
    INCIDENT_RESPONSE.name,
    DELIVERABLE_QA.name,
    CADENCE_APP_DEV.name,
    ASSISTANT_MEMORY.name,
];

/// Short one-line description for each bundled skill, matching the agent page UI.
pub fn bundled_line(name: &str) -> &'static str {
    match name {
        "cadence" => "Tickets, threads and reports",
        "business-documents" => "Document intake, templates and pricing checks",
        "company-outputs" => "Quotes, invoices, proposals and reports",
        "website-builder" => "Build and publish a static website",
        "incident-response" => "Contain and close out incidents across teams",
        "deliverable-qa" => "Independent browser and visual QA before release",
        "cadence-app-dev" => "Build, audit and package sandboxed Cadence apps",
        "assistant-memory" => "Personal SOUL.md, MEMORY.md and memory search discipline",
        _ => "Bundled skill",
    }
}

fn bundle_by_name(name: &str) -> Option<&'static BundledSkill> {
    match name {
        "cadence" => Some(&CADENCE),
        "business-documents" => Some(&BUSINESS_DOCUMENTS),
        "company-outputs" => Some(&COMPANY_OUTPUTS),
        "website-builder" => Some(&WEBSITE_BUILDER),
        "incident-response" => Some(&INCIDENT_RESPONSE),
        "deliverable-qa" => Some(&DELIVERABLE_QA),
        "cadence-app-dev" => Some(&CADENCE_APP_DEV),
        "assistant-memory" => Some(&ASSISTANT_MEMORY),
        _ => None,
    }
}

/// CAD-2092: validate a per-agent `params.skills` allowlist.
/// Every entry must be a known bundled skill, and the required `cadence`
/// skill cannot be turned off.
pub fn validate_skill_selection(names: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if bundle_by_name(name).is_none() {
            return Err(Error::rejected(format!(
                "unknown bundled skill {name:?}; expected {}",
                SKILL_NAMES.join(", ")
            )));
        }
        if !out.contains(name) {
            out.push(name.clone());
        }
    }
    if !out.iter().any(|n| n == REQUIRED_SKILL) {
        return Err(Error::rejected(format!(
            "required skill '{REQUIRED_SKILL}' cannot be turned off"
        )));
    }
    Ok(out)
}

/// CAD-2092: parse a JSON `params.skills` value into a validated skill list.
pub fn validate_skills_param(value: &Value) -> Result<Vec<String>> {
    let items = value
        .as_array()
        .ok_or_else(|| Error::rejected("'skills' must be an array of bundled skill names"))?;
    let mut names = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.as_str() else {
            return Err(Error::rejected("'skills' entries must be strings"));
        };
        names.push(name.to_string());
    }
    validate_skill_selection(&names)
}

/// CAD-2092: bundled skill catalog including exact `SKILL.md` bytes and file list
/// so the agent page drawer shows what the agent sees.
pub fn bundled_catalog() -> Vec<Value> {
    SKILL_NAMES
        .iter()
        .filter_map(|&name| {
            let bundle = bundle_by_name(name)?;
            let skill_md = bundle
                .files
                .iter()
                .find(|(rel, _)| *rel == "SKILL.md")
                .map(|(_, body)| *body)
                .unwrap_or("");
            let files: Vec<&str> = bundle.files.iter().map(|(rel, _)| *rel).collect();
            Some(json!({
                "name": bundle.name,
                "line": bundled_line(bundle.name),
                "source": "bundled",
                "bundled": true,
                "required": bundle.name == REQUIRED_SKILL,
                "skill_md": skill_md,
                "files": files,
            }))
        })
        .collect()
}

/// CAD-2092: stage ONLY the selected bundled skills into an agent's private
/// directory (`<state_dir>/agents/<alias>/skills/<name>`), removing any
/// previously staged unselected skill directories. Never touches `$HOME`.
pub fn stage_agent_skills(
    state_dir: &Path,
    alias: &str,
    selected: &[String],
) -> Result<Vec<(String, PathBuf)>> {
    crate::proto::identifier(alias, "agent alias")?;
    let validated = validate_skill_selection(selected)?;
    let agent_dir = state_dir.join("agents").join(alias);
    let root = agent_dir.join("skills");
    if root
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err(Error::rejected(format!(
            "refusing to stage skills into symlink {}",
            root.display()
        )));
    }
    std::fs::create_dir_all(&root)
        .map_err(|e| Error::provider(format!("create {}: {e}", root.display())))?;
    let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let keep = name
                .to_str()
                .is_some_and(|s| validated.iter().any(|want| want == s));
            if !keep {
                let path = entry.path();
                if path
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_dir())
                {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    let mut staged = Vec::with_capacity(validated.len());
    for name in validated {
        let bundle = bundle_by_name(&name).expect("validated skill exists");
        let dir = root.join(&name);
        if dir
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err(Error::rejected(format!(
                "refusing to write staged skill symlink {}",
                dir.display()
            )));
        }
        for (relative, content) in bundle.files {
            let file = dir.join(relative);
            if file
                .symlink_metadata()
                .is_ok_and(|meta| meta.file_type().is_symlink())
            {
                return Err(Error::rejected(format!(
                    "refusing to write staged skill file symlink {}",
                    file.display()
                )));
            }
            let stale = std::fs::read(&file)
                .map(|bytes| bytes != content.as_bytes())
                .unwrap_or(true);
            if stale {
                let parent = file.parent().expect("bundled file has a parent");
                std::fs::create_dir_all(parent)
                    .map_err(|e| Error::provider(format!("create {}: {e}", parent.display())))?;
                std::fs::write(&file, content)
                    .map_err(|e| Error::provider(format!("write {}: {e}", file.display())))?;
            }
        }
        staged.push((name, dir));
    }
    Ok(staged)
}

/// Provider skill directories that receive links to each selected bundle.
pub(super) const LINK_PARENTS: [&str; 4] = [
    ".claude/skills",
    ".cursor/skills",
    ".copilot/skills",
    ".gemini/skills",
];

pub(super) fn canonical_root(home: &Path) -> PathBuf {
    home.join(".agents").join("skills")
}

/// Real provider dirs/files are never touched. Only missing or symlink
/// entries are managed.
pub(super) fn link_state(link: &Path, target: &Path) -> &'static str {
    match std::fs::symlink_metadata(link) {
        Err(_) => "missing",
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(link) {
            Ok(t) if t == target => "ok",
            _ => "wrong-target",
        },
        Ok(_) => "foreign",
    }
}

pub(super) fn selected(target: Option<&str>) -> Result<&'static [BundledSkill]> {
    match target {
        None => Ok(&[
            CADENCE,
            BUSINESS_DOCUMENTS,
            COMPANY_OUTPUTS,
            WEBSITE_BUILDER,
            INCIDENT_RESPONSE,
            DELIVERABLE_QA,
            CADENCE_APP_DEV,
            ASSISTANT_MEMORY,
        ]),
        Some("cadence") => Ok(&[CADENCE]),
        Some("business-documents") => Ok(&[BUSINESS_DOCUMENTS]),
        Some("company-outputs") => Ok(&[BUSINESS_DOCUMENTS, COMPANY_OUTPUTS]),
        Some("website-builder") => Ok(&[WEBSITE_BUILDER]),
        Some("incident-response") => Ok(&[INCIDENT_RESPONSE]),
        Some("deliverable-qa") => Ok(&[DELIVERABLE_QA]),
        Some("cadence-app-dev") => Ok(&[CADENCE_APP_DEV]),
        Some("assistant-memory") => Ok(&[ASSISTANT_MEMORY]),
        Some(name) => Err(Error::rejected(format!(
            "unknown bundled skill {name:?}; expected {}",
            SKILL_NAMES.join(", ")
        ))),
    }
}
/// Compare the fixed bundle under `root/.agents/skills`, without returning files.
pub fn guidance_copy(root: Option<&Path>, name: &str) -> Result<&'static str> {
    let bundle = selected(Some(name))?
        .iter()
        .find(|skill| skill.name == name)
        .expect("selected skill includes its requested bundle");
    let Some(root) = root else {
        return Ok("unknown");
    };
    let root = canonical_root(root).join(name);
    let mut state = "current";
    for (relative, expected) in bundle.files {
        let actual = (|| -> std::io::Result<bool> {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(root.join(relative))?;
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            if metadata.len() != expected.len() as u64 {
                return Ok(false);
            }
            let mut buffer = [0; 4096];
            for bytes in expected.as_bytes().chunks(buffer.len()) {
                let chunk = &mut buffer[..bytes.len()];
                file.read_exact(chunk)?;
                if bytes != &*chunk {
                    return Ok(false);
                }
            }
            Ok(file.read(&mut buffer[..1])? == 0)
        })();
        match actual {
            Ok(false) if state == "current" => state = "stale",
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => state = "missing",
            Err(_) => return Ok("unknown"),
            _ => {}
        }
    }
    Ok(state)
}

/// Compare one bundled skill in this caller's home, without exposing its contents.
pub fn guidance(home: Option<&Path>, name: &str) -> Result<Value> {
    let state = guidance_copy(home, name)?;
    Ok(json!({
        "skill": name,
        "status": state,
        "scope": "caller HOME",
        "remedy": (state != "current").then(|| format!("cadence skill install {name} (review local changes first)")),
        "live_session": "not checked"
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidance_checks_only_the_requested_bundle_bytes() {
        let home = tempfile::tempdir().unwrap();
        sync(home.path(), false, Some("deliverable-qa")).unwrap();
        assert_eq!(
            guidance(Some(home.path()), "cadence").unwrap()["status"],
            "missing"
        );
        sync(home.path(), false, Some("cadence")).unwrap();
        assert_eq!(
            guidance(Some(home.path()), "cadence").unwrap()["status"],
            "current"
        );
        std::fs::write(
            canonical_root(home.path()).join("cadence/SKILL.md"),
            b"different",
        )
        .unwrap();
        assert_eq!(
            guidance(Some(home.path()), "cadence").unwrap()["status"],
            "stale"
        );
        assert_eq!(guidance(None, "cadence").unwrap()["status"], "unknown");
    }

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

    #[test]
    fn incident_response_bundle_installs_skill_and_generic_postmortem_template() {
        assert!(SKILL_NAMES.contains(&"incident-response"));
        let dir = tempfile::tempdir().unwrap();
        let first = sync(dir.path(), true, Some("incident-response")).unwrap();
        assert_eq!(first["wrote"], true);
        let root = canonical_root(dir.path()).join("incident-response");
        assert!(root.join("SKILL.md").is_file());
        assert!(root.join("references/postmortem-template.html").is_file());

        let second = sync(dir.path(), false, Some("incident-response")).unwrap();
        assert_eq!(second["wrote"], false);

        match show(
            "incident-response",
            Some("references/postmortem-template.html"),
        )
        .unwrap()
        {
            ShowOutput::File(html) => {
                assert!(html.contains("--doc-accent"));
                assert!(html.contains("@page"));
            }
            ShowOutput::Manifest(_) => panic!("file expected"),
        }
    }

    #[test]
    fn deliverable_qa_bundle_installs_one_procedure_with_the_gate_pass_lines() {
        assert!(SKILL_NAMES.contains(&"deliverable-qa"));
        let dir = tempfile::tempdir().unwrap();
        let first = sync(dir.path(), true, Some("deliverable-qa")).unwrap();
        assert_eq!(first["wrote"], true);
        let root = canonical_root(dir.path()).join("deliverable-qa");
        assert!(root.join("SKILL.md").is_file());
        let second = sync(dir.path(), false, Some("deliverable-qa")).unwrap();
        assert_eq!(second["wrote"], false);
        match show("deliverable-qa", Some("SKILL.md")).unwrap() {
            ShowOutput::File(text) => {
                // The exact two lines the merge gate counts (scripts/enqueue-reviewed).
                assert!(text.contains("Browser QA: pass\n   Head: <full 40-hex sha>"));
                assert!(text.contains("never approves on a person's behalf"));
            }
            ShowOutput::Manifest(_) => panic!("file expected"),
        }
    }

    #[test]
    fn cadence_app_dev_bundle_installs_skill_references_and_gemini_provider_link() {
        assert!(SKILL_NAMES.contains(&"cadence-app-dev"));
        let dir = tempfile::tempdir().unwrap();
        let first = sync(dir.path(), true, Some("cadence-app-dev")).unwrap();
        assert_eq!(first["wrote"], true);
        let root = canonical_root(dir.path()).join("cadence-app-dev");
        for relative in [
            "SKILL.md",
            "references/contracts-and-storage.md",
            "references/bridge-and-ui.md",
            "references/release-and-catalog.md",
        ] {
            assert!(root.join(relative).is_file(), "missing {relative}");
        }
        for parent in [
            ".claude/skills",
            ".cursor/skills",
            ".copilot/skills",
            ".gemini/skills",
        ] {
            let link = dir.path().join(parent).join("cadence-app-dev");
            assert!(
                link.symlink_metadata().unwrap().file_type().is_symlink(),
                "expected symlink at {}",
                link.display()
            );
            assert_eq!(std::fs::read_link(&link).unwrap(), root);
        }
        let second = sync(dir.path(), false, Some("cadence-app-dev")).unwrap();
        assert_eq!(second["wrote"], false);
        let report = status(dir.path());
        let entry = report["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == "cadence-app-dev")
            .expect("cadence-app-dev status entry");
        assert_eq!(entry["installed"], true);
        assert_eq!(entry["content_match"], true);
        for provider in ["claude", "cursor", "copilot", "gemini"] {
            assert_eq!(entry["links"][provider], "ok", "provider {provider}");
        }
        match show("cadence-app-dev", Some("SKILL.md")).unwrap() {
            ShowOutput::File(text) => {
                assert!(text.contains("createScreenBridge"));
                assert!(text.contains("preventDefault"));
            }
            ShowOutput::Manifest(_) => panic!("file expected"),
        }
        match show(
            "cadence-app-dev",
            Some("references/contracts-and-storage.md"),
        )
        .unwrap()
        {
            ShowOutput::File(text) => {
                assert!(text.contains("app-storage/v1"));
                assert!(text.contains("capability_slots"));
                assert!(text.contains("agent-side and unattested"));
            }
            ShowOutput::Manifest(_) => panic!("file expected"),
        }
    }

    #[test]
    fn cad2092_stage_agent_skills_exposes_only_selected_and_enforces_required() {
        let state = tempfile::tempdir().unwrap();
        // Turning off required `cadence` is refused.
        let err = stage_agent_skills(state.path(), "worker-1", &["deliverable-qa".to_string()])
            .unwrap_err();
        assert!(err.to_string().contains("required skill 'cadence'"));

        // Unknown / path-traversal names are refused.
        assert!(stage_agent_skills(
            state.path(),
            "worker-1",
            &["cadence".to_string(), "../escape".to_string()],
        )
        .is_err());

        // Stage two skills: only those two exist under agents/worker-1/skills.
        let first = stage_agent_skills(
            state.path(),
            "worker-1",
            &["cadence".to_string(), "deliverable-qa".to_string()],
        )
        .unwrap();
        assert_eq!(first.len(), 2);
        let root = state.path().join("agents/worker-1/skills");
        assert!(root.join("cadence/SKILL.md").is_file());
        assert!(root.join("deliverable-qa/SKILL.md").is_file());
        assert!(!root.join("website-builder").exists());

        // Narrowing selection prunes previously staged unselected skills.
        let second =
            stage_agent_skills(state.path(), "worker-1", &["cadence".to_string()]).unwrap();
        assert_eq!(second.len(), 1);
        assert!(root.join("cadence/SKILL.md").is_file());
        assert!(!root.join("deliverable-qa").exists());
    }

    #[test]
    fn assistant_memory_bundle_registers_and_shows() {
        assert!(SKILL_NAMES.contains(&"assistant-memory"));
        match show("assistant-memory", Some("SKILL.md")).unwrap() {
            ShowOutput::File(text) => {
                assert!(text.contains("cadence assist memory search"));
                assert!(text.contains("cadence assist soul set"));
            }
            ShowOutput::Manifest(_) => panic!("file expected"),
        }
    }
}
