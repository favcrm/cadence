//! Per-issue field, contradiction, item-type, cycle, and link-context checks
//! for `issue::lint`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::{HashMap, HashSet};

use crate::issue::{model, parse, project, Pm};

use super::{Fronts, Lint};

pub(super) fn check_issues(
    pm: &Pm,
    projects: &[project::Project],
    fronts: &Fronts,
    work_configs: &HashMap<String, crate::issue::work::WorkConfig>,
    checked: &HashSet<String>,
    lint: &mut Lint,
) {
    // Per-issue field checks + link targets.
    for (project_key, front, folder) in fronts.values().filter(|(_, f, _)| checked.contains(&f.id))
    {
        let id = front.id.as_str();
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
        if let Some(plan) = &front.plan {
            if !model::PLAN_STATES.contains(&plan.state.as_str()) {
                lint.err(format!("{id}: unknown plan state '{}'", plan.state));
            }
        }
        if let Some(size) = &front.size {
            if !model::SIZES.iter().any(|(s, _)| s == size) {
                lint.err(format!("{id}: unknown size '{size}' — S, M or L"));
            }
        }
        // CAD-405 work-model fields. A bad `type` or milestone id is an
        // error like a bad status; a mismatch with PROJECT.md (which can
        // change under existing issues) only warns.
        if let Some(t) = &front.item_type {
            if !model::TYPES.contains(&t.as_str()) {
                lint.err(format!(
                    "{id}: unknown type '{t}' — one of {}",
                    model::TYPES.join(" ")
                ));
            }
        }
        if let Some(m) = &front.milestone {
            if !model::valid_tag(m) {
                lint.err(format!("{id}: bad milestone grammar '{m}'"));
            }
        }
        if let Some(cfg) = work_configs.get(project_key) {
            if let Some(stage) = &front.stage {
                if cfg.index(stage).is_none() {
                    lint.warn(format!(
                        "{id}: stage '{stage}' is not in {project_key}'s list ({})",
                        cfg.stage_ids().join(" ")
                    ));
                }
            }
            if let Some((m, _)) = model::milestone_of(front) {
                if !cfg.milestones.is_empty() && !cfg.milestones.iter().any(|d| d.id == m) {
                    lint.warn(format!(
                        "{id}: milestone '{m}' is not declared in {project_key}/PROJECT.md"
                    ));
                }
            }
        }
        if let Some(at) = &front.stage_at {
            if crate::issue::time::parse_iso(at).is_none() {
                lint.warn(format!(
                    "{id}: stage_at '{at}' is not RFC 3339 UTC — time in stage is unknown"
                ));
            }
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
}

pub(super) fn check_contradictions(fronts: &Fronts, checked: &HashSet<String>, lint: &mut Lint) {
    // Contradiction warning: status claims progress while a blocker is
    // still open. File status is what lint audits — derivation is a
    // runtime concern.
    for (_, front, _) in fronts.values().filter(|(_, f, _)| checked.contains(&f.id)) {
        // CAD-755: the daemon stamped the claim stale — the holder's
        // agent is dead and the claim no longer counts as live work.
        if let Some(c) = front.claim.as_ref().filter(|c| c.stale.is_some()) {
            lint.warn(format!(
                "{}: claim by {} is stale — {}",
                front.id,
                c.by,
                c.stale.as_deref().unwrap_or_default()
            ));
        }
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
}

pub(super) fn check_types(fronts: &Fronts, checked: &HashSet<String>, lint: &mut Lint) {
    // CAD-405: a stage belongs to an epic; an explicit non-epic type on
    // an issue with children contradicts the tree.
    let parents: HashSet<&str> = fronts
        .values()
        .filter_map(|(_, f, _)| f.parent.as_deref())
        .collect();
    for (id, (_, front, _)) in fronts.iter().filter(|(id, _)| checked.contains(*id)) {
        let kind = model::item_type(front, parents.contains(id.as_str()));
        if kind != "epic" && front.stage.is_some() {
            lint.warn(format!(
                "{id}: has a stage but is a {kind} — only epics have stages"
            ));
        }
        if kind != "epic" && parents.contains(id.as_str()) {
            lint.warn(format!(
                "{id}: type '{kind}' but has children — epics hold tasks"
            ));
        }
    }
}

pub(super) fn check_cycles(fronts: &Fronts, checked: &HashSet<String>, lint: &mut Lint) {
    // Depth ≤ 2 and parent acyclicity.
    for (id, (_, front, _)) in fronts.iter().filter(|(id, _)| checked.contains(*id)) {
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
    for id in fronts.keys().filter(|id| checked.contains(*id)) {
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
}

/// Scoped lint: load the issues the checked ones point at, so dangling
/// targets, depth and cycles resolve exactly as in the full lint.
/// `parent` and `blocked_by` chains are followed transitively;
/// `relates` and `duplicate_of` of the checked issues are only looked up. Also the
/// cross-project duplicate-id check the folder scan cannot see.
pub(super) fn load_link_context(
    pm: &Pm,
    projects: &[project::Project],
    fronts: &mut HashMap<String, (String, model::Front, String)>,
    lint: &mut Lint,
) {
    for (id, (key, _, _)) in fronts.clone() {
        for other in projects.iter().filter(|p| p.key != key) {
            if pm.dir.join(&other.key).join(&id).join("issue.md").is_file() {
                lint.err(format!(
                    "{id}: duplicated id in {key}/{id} and {}/{id}",
                    other.key
                ));
            }
        }
    }
    let mut work: Vec<String> = Vec::new();
    for (_, f, _) in fronts.values() {
        let links = f.blocked_by.iter().chain(f.parent.iter());
        work.extend(
            links
                .chain(f.relates.iter())
                .chain(f.duplicate_of.iter())
                .cloned(),
        );
    }
    while let Some(id) = work.pop() {
        if fronts.contains_key(&id) || !model::valid_id(&id) {
            continue;
        }
        for p in projects {
            let file = pm.dir.join(&p.key).join(&id).join("issue.md");
            if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let Ok((front, _)) = parse::parse_issue(&text) else {
                continue;
            };
            work.extend(front.blocked_by.iter().chain(front.parent.iter()).cloned());
            fronts.insert(id.clone(), (p.key.clone(), front, id.clone()));
            break;
        }
    }
}
