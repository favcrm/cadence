//! The `cadence` agent skill, vendored into the binary and installed
//! under `$HOME/.agents/skills/cadence/` with `cadence` symlinks from
//! each agent CLI's skill dir (`~/.claude/skills`, `~/.cursor/skills`,
//! `~/.copilot/skills`). `daemon run` re-syncs on every start so a
//! rebuilt binary propagates skill changes.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};

/// The vendored skill document — single source of truth.
pub const SKILL_MD: &str = include_str!("../skill/cadence/SKILL.md");

/// `~/.claude/skills`-style parents that receive a `cadence` symlink.
const LINK_PARENTS: [&str; 3] = [".claude/skills", ".cursor/skills", ".copilot/skills"];

/// `$HOME/.agents/skills/cadence` — no XDG equivalent, always `$HOME`.
fn canonical_dir(home: &Path) -> PathBuf {
    home.join(".agents").join("skills").join("cadence")
}

/// One `cadence` entry inside a link parent. Real dirs/files are never
/// touched — only our symlinks and missing entries are managed.
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

/// Install or refresh the skill under `home`. With `force` the file is
/// rewritten unconditionally (explicit `skill install`); otherwise only
/// stale/missing content is written (`daemon run` startup). Symlinks are
/// created or re-pointed when missing/wrong; real entries are left alone
/// and reported as skipped. Returns `{installed, wrote, linked, skipped}`.
pub fn sync(home: &Path, force: bool) -> Result<Value> {
    let dir = canonical_dir(home);
    let file = dir.join("SKILL.md");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::provider(format!("create {}: {e}", dir.display())))?;
    let stale = std::fs::read_to_string(&file)
        .map(|s| s != SKILL_MD)
        .unwrap_or(true);
    let wrote = force || stale;
    if wrote {
        std::fs::write(&file, SKILL_MD)
            .map_err(|e| Error::provider(format!("write {}: {e}", file.display())))?;
    }
    let mut linked = Vec::new();
    let mut skipped = Vec::new();
    for parent in LINK_PARENTS {
        let skills = home.join(parent);
        let link = skills.join("cadence");
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
    Ok(json!({
        "installed": file, "wrote": wrote, "linked": linked, "skipped": skipped,
    }))
}

/// Cheap status: installed? content matches the embedded copy? which
/// link dirs are wired?
pub fn status(home: &Path) -> Value {
    let dir = canonical_dir(home);
    let file = dir.join("SKILL.md");
    let installed = file.is_file();
    let content_match = std::fs::read_to_string(&file)
        .map(|s| s == SKILL_MD)
        .unwrap_or(false);
    let links: serde_json::Map<String, Value> = LINK_PARENTS
        .iter()
        .map(|p| {
            let name = p.split('/').next().unwrap_or(p).trim_start_matches('.');
            (
                name.to_string(),
                json!(link_state(&home.join(p).join("cadence"), &dir)),
            )
        })
        .collect();
    json!({
        "installed": installed, "path": file,
        "content_match": content_match, "links": links,
    })
}
