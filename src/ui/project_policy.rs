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

/// The tracker's projects, resolved to one caller — the single read the
/// three access questions below are answered from.
///
/// The fail-closed inputs live here, once: an unreadable tracker is `None`
/// rather than an empty list (so no answer can read it as "nothing is
/// restricted"), and a spelling outside the known forms resolves to no
/// caller at all. Each question then states its own answer, including how
/// it treats an empty or unresolvable read.
struct Access<'a> {
    caller: Option<project::ProjectCaller<'a>>,
    projects: Option<Vec<project::Project>>,
}

impl<'a> Access<'a> {
    fn of(pm: &Pm, actor: &'a str) -> Self {
        Self {
            caller: caller_of(actor),
            // CAD-1950: `list` answers an empty vector for an unreadable
            // tracker, and an empty vector reads as "nothing is restricted".
            // The checked listing errors instead, and `None` is the fail-closed
            // answer every question below already handles.
            projects: project::list_checked(&pm.dir).ok(),
        }
    }

    /// The keys this caller may read; empty when either half is missing.
    fn readable(&self) -> BTreeSet<String> {
        let (Some(caller), Some(projects)) = (self.caller, self.projects.as_deref()) else {
            return BTreeSet::new();
        };
        projects
            .iter()
            .filter(|p| p.allows(caller))
            .map(|p| p.key.clone())
            .collect()
    }

    /// True when `key` exists and its policy does not admit this caller. An
    /// absent project is not this guard's business.
    fn refused(&self, key: &str) -> bool {
        let Some(projects) = self.projects.as_deref() else {
            return true;
        };
        let Some(found) = projects.iter().find(|p| p.key == key) else {
            return false;
        };
        match self.caller {
            Some(caller) => !found.allows(caller),
            None => true,
        }
    }

    /// True when this caller may read every project in the tracker.
    fn reads_every_project(&self) -> bool {
        let (Some(caller), Some(projects)) = (self.caller, self.projects.as_deref()) else {
            return false;
        };
        projects.iter().all(|p| p.allows(caller))
    }
}

/// The project keys this caller may read.
///
/// Fail closed: an unknown spelling or an unreadable tracker yields an
/// empty set, so a listing contributes no row, count or placeholder for a
/// project the caller may not see.
pub(super) fn readable(pm: &Pm, actor: &str) -> BTreeSet<String> {
    Access::of(pm, actor).readable()
}

/// True when `key` exists and its policy does not admit this caller.
///
/// An absent project returns false, so the caller's verb keeps its own
/// unknown-project answer. An unreadable tracker refuses.
pub(super) fn refused(pm: &Pm, key: &str, actor: &str) -> bool {
    Access::of(pm, actor).refused(key)
}

/// True when this caller may read every project in the tracker.
///
/// The board's live stream is one shared `Model` whose frames carry
/// issue/agent/plan rows and aggregate counts, and it cannot yet be
/// filtered per subscriber. Serving it to a caller who cannot read every
/// project would leak rows and counts from the restricted ones, so that
/// caller is refused live updates instead (AOS-274 / SEC-006). Fail
/// closed: an unreadable tracker or unknown spelling is not complete.
pub(super) fn reads_every_project(pm: &Pm, actor: &str) -> bool {
    Access::of(pm, actor).reads_every_project()
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
    fn only_a_caller_that_reads_every_project_gets_the_shared_stream() {
        let (_dir, pm) = pm_with(&[("demo", PRIVATE), ("open", "")]);
        // The Owner reads everything; the granted member does too only
        // because the restricted project lists them.
        assert!(reads_every_project(&pm, "operator"));
        assert!(reads_every_project(&pm, "users/mia"));
        // An unselected member, a reserved kind and an unknown spelling
        // are all refused the shared stream.
        for spelling in ["users/stranger", "agents/dev", "service/svc", "mystery"] {
            assert!(
                !reads_every_project(&pm, spelling),
                "{spelling} was offered the shared stream"
            );
        }
        // With no recorded policy every project is readable, so the
        // stream is unrestricted exactly as before.
        let (_dir, pm) = pm_with(&[("demo", "")]);
        assert!(reads_every_project(&pm, "users/anyone"));
    }

    /// CAD-1950: an unreadable tracker is not an empty one.
    ///
    /// `project::list` answers `Ok(empty)` when `read_dir` fails, and an empty
    /// list reads as "nothing is restricted" — so every access question would
    /// fail open. The access layer uses the checked listing, and this test
    /// pins both halves: the plain listing still answers empty (its other
    /// callers render what they could read), and the policy layer refuses.
    #[test]
    fn an_unreadable_tracker_refuses_every_project() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, pm) = pm_with(&[("demo", PRIVATE), ("open", "")]);
        let tracker = pm.dir.clone();
        std::fs::set_permissions(&tracker, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_dir(&tracker).is_ok() {
            // Root (or an ACL) can read it anyway, so the mode cannot express
            // this case here; leave the tracker usable and skip.
            std::fs::set_permissions(&tracker, std::fs::Permissions::from_mode(0o755)).unwrap();
            drop(dir);
            return;
        }

        // The plain listing is unchanged for its rendering callers…
        assert!(
            project::list(&tracker).unwrap().is_empty(),
            "list still answers empty for an unreadable tracker"
        );
        // …and the checked one refuses to describe it as complete.
        assert!(
            project::list_checked(&tracker).is_err(),
            "the checked listing must not answer empty for an unreadable tracker"
        );

        // Every access question fails closed, including for the owner.
        assert!(readable(&pm, "operator").is_empty());
        assert!(
            refused(&pm, "demo", "operator"),
            "a project that cannot be checked must be refused, not admitted"
        );
        assert!(!reads_every_project(&pm, "operator"));

        std::fs::set_permissions(&tracker, std::fs::Permissions::from_mode(0o755)).unwrap();
        // And the answers return once it can be read again.
        assert!(!refused(&pm, "demo", "operator"));
        assert!(readable(&pm, "operator").contains("demo"));
    }

    /// CAD-1950: a project directory that exists but cannot be searched hides
    /// its `project.yaml` behind `PermissionDenied`. `board::is_real_file`
    /// turns that into `false`, which would omit the project and call the
    /// listing complete — the same fail-open one level down. The checked
    /// listing must refuse instead.
    #[test]
    fn an_uninspectable_project_refuses_the_whole_listing() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, pm) = pm_with(&[("demo", PRIVATE), ("open", "")]);
        let hidden = pm.dir.join("demo");
        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::symlink_metadata(hidden.join("project.yaml")).is_ok() {
            // Root (or an ACL) can still inspect it; the mode cannot express
            // this case here.
            std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o755)).unwrap();
            drop(dir);
            return;
        }

        // The lenient listing omits it and answers a complete-looking set…
        let lenient = project::list(&pm.dir).unwrap();
        assert!(
            lenient.iter().all(|p| p.key != "demo"),
            "list still omits an uninspectable project"
        );
        // …which the checked listing refuses to do.
        assert!(
            project::list_checked(&pm.dir).is_err(),
            "an uninspectable project must not be silently omitted"
        );
        // So a caller that cannot be checked is refused, even the owner.
        assert!(refused(&pm, "demo", "operator"));
        assert!(readable(&pm, "operator").is_empty());

        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!refused(&pm, "demo", "operator"));
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
