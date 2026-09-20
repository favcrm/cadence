//! `issue lint` — the pre-commit-hook check over the whole PM dir.
//! Schema, id/folder match, dangling and cyclic links, depth, oversize
//! artifacts, unknown status/kind. Non-zero exit on any error.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::error::Result;
use crate::issue::{model, parse, project, Pm};

struct Lint {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Lint {
    fn err(&mut self, msg: impl Into<String>) {
        self.errors.push(msg.into());
    }
    fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }
}

pub fn run(pm: &Pm, only_project: Option<&str>) -> Result<Value> {
    let mut lint = Lint {
        errors: vec![],
        warnings: vec![],
    };
    let projects = project::list(&pm.dir)?;
    // project::list skips symlinked dirs — flag them here instead of
    // letting them disappear silently.
    if let Ok(entries) = std::fs::read_dir(&pm.dir) {
        for entry in entries.flatten() {
            let ft = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
            if ft {
                lint.err(format!(
                    "{}: symlinked entry — the board never follows links",
                    entry.file_name().to_string_lossy()
                ));
            }
        }
    }
    let mut fronts: HashMap<String, (String, model::Front, String)> = HashMap::new();
    for project in &projects {
        if let Some(only) = only_project {
            if project.key != only {
                continue;
            }
        }
        let dir = pm.dir.join(&project.key);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            // DirEntry::file_type is lstat-style — a symlink is not a dir.
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                lint.err(format!(
                    "{}/{name}: symlinked issue folder — the board never follows links",
                    project.key
                ));
                continue;
            }
            if !ft.is_dir() || name.starts_with('.') {
                continue;
            }
            // `memory/` holds project-memory files, not an issue —
            // its own lint validates them against the body contract.
            if name == "memory" {
                let (mut merrs, mut mwarns) = (Vec::new(), Vec::new());
                crate::memory::lint_dir(&path, project, &mut |e| merrs.push(e), &mut |w| {
                    mwarns.push(w)
                });
                for e in merrs {
                    lint.err(e);
                }
                for w in mwarns {
                    lint.warn(w);
                }
                continue;
            }
            let file = path.join("issue.md");
            if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
                lint.err(format!(
                    "{}/{name}: issue.md is a symlink — the board never follows links",
                    project.key
                ));
                continue;
            }
            if !file.is_file() {
                lint.err(format!("{}/{}: no issue.md", project.key, name));
                continue;
            }
            for sub in ["comments", "artifacts"] {
                let sub_dir = path.join(sub);
                if sub_dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
                    lint.err(format!(
                        "{}/{name}: {sub}/ is a symlink — the board never follows links",
                        project.key
                    ));
                    continue;
                }
                if let Ok(files) = std::fs::read_dir(&sub_dir) {
                    for f in files.flatten() {
                        if f.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                            lint.err(format!(
                                "{}/{name}: {sub}/{} is a symlink",
                                project.key,
                                f.file_name().to_string_lossy()
                            ));
                        }
                    }
                }
            }
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            match parse::parse_issue(&text) {
                Ok((front, _body)) => {
                    if front.id != name {
                        lint.err(format!(
                            "{}/{}: frontmatter id '{}' does not match folder",
                            project.key, name, front.id
                        ));
                    }
                    fronts.insert(front.id.clone(), (project.key.clone(), front, name));
                }
                Err(e) => lint.err(format!("{}/{name}: {e}", project.key)),
            }
        }
    }

    // Per-issue field checks + link targets.
    let mut seen_ids = HashSet::new();
    for (project_key, front, folder) in fronts.values() {
        let id = front.id.as_str();
        if !seen_ids.insert(id.to_string()) {
            lint.err(format!("{id}: duplicated id"));
        }
        if !model::valid_id(id) {
            lint.err(format!("{project_key}/{folder}: bad id grammar '{id}'"));
        }
        if let Some(project) = projects.iter().find(|p| p.key == *project_key) {
            if !id.starts_with(&format!("{}-", project.prefix)) {
                lint.err(format!(
                    "{id}: prefix does not match project '{}' ({})",
                    project.key, project.prefix
                ));
            }
            if let Some(comp) = &front.component {
                if !project.components.is_empty() && !project.components.contains(comp) {
                    lint.err(format!("{id}: unknown component '{comp}'"));
                }
            }
            for tag in &front.tags {
                if !model::valid_tag(tag) {
                    lint.err(format!("{id}: bad tag grammar '{tag}'"));
                } else if !project.tags.is_empty()
                    && !project.tags.contains(tag)
                    && !model::system_tag(tag)
                {
                    lint.err(format!("{id}: unknown tag '{tag}'"));
                }
            }
        }
        let mut seen_tags = HashSet::new();
        for tag in &front.tags {
            if !seen_tags.insert(tag.as_str()) {
                lint.err(format!("{id}: duplicated tag '{tag}'"));
            }
        }
        if front.tags.len() > model::TAG_MAX {
            lint.err(format!(
                "{id}: {} tags (cap {})",
                front.tags.len(),
                model::TAG_MAX
            ));
        }
        if !model::STATUSES.contains(&front.status.as_str()) {
            lint.err(format!("{id}: unknown status '{}'", front.status));
        }
        if !model::PRIORITIES.contains(&front.priority.as_str()) {
            lint.err(format!("{id}: unknown priority '{}'", front.priority));
        }
        for r in &front.refs {
            if !model::REF_KINDS.contains(&r.kind.as_str()) {
                lint.err(format!("{id}: unknown ref kind '{}'", r.kind));
            }
            if r.url.is_none() && r.path.is_none() {
                lint.err(format!("{id}: ref '{}' has neither url nor path", r.kind));
            }
        }
        for dep in front.blocked_by.iter().chain(front.relates.iter()) {
            if !fronts.contains_key(dep) {
                lint.err(format!("{id}: dangling link target '{dep}'"));
            }
            if dep == id {
                lint.err(format!("{id}: links to itself"));
            }
        }
        if let Some(p) = &front.parent {
            if p == id {
                lint.err(format!("{id}: is its own parent"));
            } else if !fronts.contains_key(p) {
                lint.err(format!("{id}: dangling parent '{p}'"));
            }
        }
        if let Some(d) = &front.duplicate_of {
            if d == id {
                lint.err(format!("{id}: duplicate_of itself"));
            } else if !fronts.contains_key(d) {
                lint.err(format!("{id}: dangling duplicate_of '{d}'"));
            }
        }
        // Oversize artifacts.
        let artifacts = pm.dir.join(project_key).join(&front.id).join("artifacts");
        if let Ok(entries) = std::fs::read_dir(&artifacts) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.len() > pm.config.artifact_max_bytes {
                        lint.err(format!(
                            "{id}: artifact '{}' is {} bytes (cap {})",
                            entry.file_name().to_string_lossy(),
                            meta.len(),
                            pm.config.artifact_max_bytes
                        ));
                    }
                }
            }
        }
    }

    // Contradiction warning: status claims progress while a blocker is
    // still open. File status is what lint audits — derivation is a
    // runtime concern.
    for (_, front, _) in fronts.values() {
        if matches!(front.status.as_str(), "ready" | "doing" | "review") {
            let open: Vec<&str> = front
                .blocked_by
                .iter()
                .filter(|dep| {
                    fronts
                        .get(dep.as_str())
                        .map(|(_, f, _)| !matches!(f.status.as_str(), "done" | "dropped"))
                        .unwrap_or(false)
                })
                .map(String::as_str)
                .collect();
            if !open.is_empty() {
                lint.warn(format!(
                    "{}: status '{}' but blocked_by {} still open",
                    front.id,
                    front.status,
                    open.join(", ")
                ));
            }
        }
    }

    // Depth ≤ 2 and parent acyclicity.
    for (id, (_, front, _)) in &fronts {
        let mut depth = 1;
        let mut cur = front;
        let mut seen = HashSet::from([id.as_str()]);
        while let Some(up) = &cur.parent {
            depth += 1;
            if depth > 2 {
                lint.err(format!("{id}: parent chain exceeds depth 2"));
                break;
            }
            if !seen.insert(up.as_str()) {
                lint.err(format!("{id}: parent cycle through '{up}'"));
                break;
            }
            match fronts.get(up) {
                Some((_, f, _)) => cur = f,
                None => break,
            }
        }
    }
    // blocked_by cycles (DFS per node over resolved targets).
    for id in fronts.keys() {
        let mut seen = HashSet::new();
        let mut stack = vec![id.as_str()];
        let mut cycle = false;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            if let Some((_, f, _)) = fronts.get(cur) {
                for dep in &f.blocked_by {
                    if dep == id {
                        cycle = true;
                    }
                    stack.push(dep);
                }
            }
        }
        if cycle {
            lint.err(format!("{id}: blocked_by cycle back to itself"));
        }
    }

    Ok(json!({
        "ok": lint.errors.is_empty(),
        "errors": lint.errors,
        "warnings": lint.warnings,
        "projects": projects.len(),
        "issues": fronts.len(),
    }))
}
