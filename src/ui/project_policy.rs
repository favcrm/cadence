//! AOS-274: the one project-access decision shared by every board surface.
//!
//! The policy itself lives with the project (see [`crate::issue::project`]).
//! This module only answers "may this caller use this project?" in the
//! daemon's caller spelling, so the CLI route and the board cannot drift
//! into two authorization conventions. Every answer fails closed: an
//! unknown spelling, an unreadable tracker or an unparsable policy is a
//! refusal, never an allow.

use std::collections::BTreeSet;

use crate::issue::{project, Pm};

/// The project caller an actor spelling names.
///
/// `None` for a spelling outside the known forms — the caller is then
/// refused rather than guessed at. The reserved `agents/` and `service/`
/// kinds map to their own variants so a restricted project refuses them
/// explicitly.
pub(super) fn caller_of(actor: &str) -> Option<project::ProjectCaller<'_>> {
    use project::ProjectCaller;
    if actor == "operator" {
        return Some(ProjectCaller::Operator);
    }
    if let Some(handle) = actor.strip_prefix("users/") {
        return Some(ProjectCaller::User(handle));
    }
    if let Some(id) = actor.strip_prefix("agents/") {
        return Some(ProjectCaller::Agent(id));
    }
    if let Some(sub) = actor.strip_prefix("service/") {
        return Some(ProjectCaller::Service(sub));
    }
    None
}

/// The project keys this caller may read.
///
/// Fail closed: an unknown spelling or an unreadable tracker yields an
/// empty set, so a listing contributes no row, count or placeholder for a
/// project the caller may not see.
pub(super) fn readable(pm: &Pm, actor: &str) -> BTreeSet<String> {
    let Some(caller) = caller_of(actor) else {
        return BTreeSet::new();
    };
    project::list(&pm.dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.allows(caller))
        .map(|p| p.key)
        .collect()
}

/// True when `key` exists and its policy does not admit this caller.
///
/// An absent project returns false, so the caller's verb keeps its own
/// unknown-project answer. An unreadable tracker refuses.
pub(super) fn refused(pm: &Pm, key: &str, actor: &str) -> bool {
    let Ok(projects) = project::list(&pm.dir) else {
        return true;
    };
    let Some(found) = projects.iter().find(|p| p.key == key) else {
        return false;
    };
    match caller_of(actor) {
        Some(caller) => !found.allows(caller),
        None => true,
    }
}

/// Drop project-scoped rows the caller may not read from an overview
/// payload.
///
/// `needs_me` rows name their project; `projects` rows name it by `key`.
/// Any other array element that names a `project` is filtered too, so a
/// section added later fails closed instead of leaking. A row with no
/// project identity is not project-scoped and stays — except in
/// `projects`, where an unnamed row is dropped.
pub(super) fn retain_overview(payload: &mut serde_json::Value, readable: &BTreeSet<String>) {
    let Some(root) = payload.as_object_mut() else {
        return;
    };
    for (name, value) in root.iter_mut() {
        let Some(rows) = value.as_array_mut() else {
            continue;
        };
        rows.retain(|row| {
            let named = row
                .get("project")
                .and_then(|v| v.as_str())
                .or_else(|| row.get("key").and_then(|v| v.as_str()));
            match named {
                Some(project) => readable.contains(project),
                None => name != "projects",
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pm_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Pm) {
        let dir = tempfile::tempdir().unwrap();
        Pm::init(dir.path()).unwrap();
        for (key, policy) in files {
            let project = dir.path().join(key);
            std::fs::create_dir_all(&project).unwrap();
            std::fs::write(
                project.join("project.yaml"),
                format!("key: {key}\nprefix: {}\n{policy}", key.to_uppercase()),
            )
            .unwrap();
        }
        let pm = Pm::at(dir.path()).unwrap();
        (dir, pm)
    }

    const PRIVATE: &str =
        "policy:\n  version: 1\n  visibility: private\n  members:\n  - users/mia\n";

    #[test]
    fn caller_spellings_map_to_the_daemon_kinds() {
        assert_eq!(
            caller_of("operator"),
            Some(project::ProjectCaller::Operator)
        );
        assert_eq!(
            caller_of("users/mia"),
            Some(project::ProjectCaller::User("mia"))
        );
        assert_eq!(
            caller_of("agents/dev"),
            Some(project::ProjectCaller::Agent("dev"))
        );
        assert_eq!(
            caller_of("service/svc"),
            Some(project::ProjectCaller::Service("svc"))
        );
        // An unknown spelling is never guessed into an allow.
        assert_eq!(caller_of("mia"), None);
        assert_eq!(caller_of(""), None);
    }

    #[test]
    fn a_restricted_project_refuses_every_other_caller() {
        let (_dir, pm) = pm_with(&[("demo", PRIVATE)]);
        assert!(!refused(&pm, "demo", "operator"));
        assert!(!refused(&pm, "demo", "users/mia"));
        for spelling in ["users/stranger", "agents/dev", "service/svc", "mystery"] {
            assert!(
                refused(&pm, "demo", spelling),
                "{spelling} was admitted to a private project"
            );
        }
        // An absent project is not this guard's business.
        assert!(!refused(&pm, "ghost", "users/stranger"));
    }

    #[test]
    fn a_listing_omits_projects_the_caller_may_not_read() {
        let (_dir, pm) = pm_with(&[("demo", PRIVATE), ("open", "")]);
        let stranger = readable(&pm, "users/stranger");
        assert!(stranger.contains("open") && !stranger.contains("demo"));

        let owner = readable(&pm, "operator");
        assert!(owner.contains("demo") && owner.contains("open"));

        // Fail closed on an unknown spelling.
        assert!(readable(&pm, "mystery").is_empty());
    }

    #[test]
    fn a_project_without_a_recorded_policy_stays_readable() {
        let (_dir, pm) = pm_with(&[("demo", "")]);
        assert!(!refused(&pm, "demo", "users/anyone"));
        assert!(readable(&pm, "users/anyone").contains("demo"));
    }

    #[test]
    fn an_overview_drops_rows_for_unreadable_projects() {
        let readable: BTreeSet<String> = ["open".to_string()].into_iter().collect();
        let mut payload = serde_json::json!({
            "needs_me": [
                {"project": "demo", "title": "hidden"},
                {"project": "open", "title": "visible"},
                {"title": "not project-scoped"},
            ],
            "projects": [{"key": "demo"}, {"key": "open"}, {"label": "unnamed"}],
            "drift": [{"project": "demo"}, {"note": "no project"}],
            "scope": {"project": null},
        });
        retain_overview(&mut payload, &readable);
        assert_eq!(payload["needs_me"].as_array().unwrap().len(), 2);
        assert_eq!(payload["projects"].as_array().unwrap().len(), 1);
        assert_eq!(payload["projects"][0]["key"], "open");
        assert_eq!(payload["drift"].as_array().unwrap().len(), 1);
        assert_eq!(payload["scope"]["project"], serde_json::Value::Null);
    }
}
