//! CAD-622: an app's own team agents cannot author changes to that app.
//!
//! Operator decision (2026-09-26): app changes are ordinary work — the
//! master or any connected agent may author them (plan → branch →
//! independent review → merge → `cadence app update` → operator
//! re-approval for structural changes). There is no dedicated "Improve
//! this app" feature. The one exception is the app's own team: an agent
//! the operator assigned to a role of an installation (`cadence app
//! set-team`, the saved team on the install record) — or that a run's
//! frozen role inputs name — never authors a change to that app's
//! source, because that is an agent widening the scope it was given.
//!
//! What counts as a change to an app's source:
//!
//! - a repo-relative path under `apps/<name>/` (a ticket's planned
//!   `paths:`, a plan that names the path, a PR's changed files);
//! - the app's git source, when the install record pinned one
//!   ([`crate::issue::app::Source::Git`]) — a PR to that repository.
//!
//! Who is on the team:
//!
//! - the saved team roles of **every** project's install record for
//!   that app name — membership is by agent alias, never by the role
//!   key, so renaming a role cannot launder an author, and a ticket in
//!   another project cannot either;
//! - with a store, the role inputs of that installation's runs and the
//!   agents those runs assigned. The run snapshot is frozen at creation,
//!   so this reads what the run actually named, never the current
//!   workflow.
//!
//! The guard is applied at the earliest seam that knows the app and the
//! author — `dispatch::run`'s preflight (the ticket's planned paths),
//! `plan_propose` (the plan's text and its proposer) — and again at the
//! review loop, where the PR's own changed files are the evidence
//! (`delivery_rpc`). The CLI-side seams read the saved team only (no
//! store connection); the daemon adds the run-time half.
//!
//! Fail closed: an install record that cannot be read, or a run snapshot
//! that cannot be parsed, refuses — it is never read as "no team".

use std::collections::BTreeSet;
use std::path::Path;

use crate::error::{Error, Result};
use crate::issue::{app, model, project};

/// The app a repo-relative path belongs to: `apps/<name>[/…]` → `<name>`.
/// A leading `./` or `/` is dropped; any other shape (a deeper `apps/`
/// component, a `..` escape, a name that is not tag-shaped) is not an
/// app path.
pub fn app_in_path(path: &str) -> Option<String> {
    let path = path.trim().trim_start_matches("./").trim_start_matches('/');
    let mut parts = path.split('/');
    if parts.next() != Some(app::DIR) {
        return None;
    }
    let name = parts.next()?;
    model::valid_tag(name).then(|| name.to_string())
}

/// Every app named by `paths` — the ticket's declared files, or a PR's
/// changed files.
pub fn apps_in_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    paths.into_iter().filter_map(app_in_path).collect()
}

/// Every app a plan's prose names as `apps/<name>/` — a plan ticket has
/// no `paths:` field, so the path it names is the only evidence a plan
/// touches an app. Only the exact `apps/<name>/` spelling counts.
pub fn apps_in_text(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find(&format!("{}/", app::DIR)) {
        rest = &rest[at + app::DIR.len() + 1..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
            .collect();
        if !name.is_empty() && rest[name.len()..].starts_with('/') && model::valid_tag(&name) {
            out.insert(name);
        }
    }
    out
}

/// `(project, app)` for every install record in the tracker — the
/// enumeration the guard walks, so a ticket in another project cannot
/// hide a team from it. A record that cannot be listed is skipped; a
/// record that cannot be read refuses in [`installs`].
fn installed_apps(pm_dir: &Path) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for p in project::list(pm_dir)? {
        let dir = pm_dir.join(&p.key).join(app::DIR);
        if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix(".yaml") else {
                continue;
            };
            if model::valid_tag(stem) {
                out.push((p.key.clone(), stem.to_string()));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Every installed app's team in one walk: app name → the agents on its
/// team in any project. The saved team roles, plus — when a store is
/// given — each installation's run role inputs and the agents its runs
/// assigned. An unreadable record or run snapshot refuses: "no team" is
/// never inferred from a file the guard could not read.
pub fn teams(
    pm_dir: &Path,
    store: Option<&crate::store::Store>,
) -> Result<std::collections::BTreeMap<String, BTreeSet<String>>> {
    let mut out: std::collections::BTreeMap<String, BTreeSet<String>> = Default::default();
    for (project_key, app_name) in installed_apps(pm_dir)? {
        let record = app::read_record(pm_dir, &project_key, &app_name)?;
        let team = out.entry(app_name.clone()).or_default();
        team.extend(record.team.values().cloned());
        let Some(store) = store else { continue };
        // The roles a run input can name: the saved team's keys and the
        // roles the app's workflows declare. An unreadable workflow only
        // drops that source; the saved keys still cover it.
        let mut roles: BTreeSet<String> = record.team.keys().cloned().collect();
        roles.extend(app::team_roles(pm_dir, &project_key, &app_name).unwrap_or_default());
        for (inputs, assigned) in store.app_run_team_inputs(&record.install_id)? {
            team.extend(assigned);
            for (role, alias) in inputs {
                if roles.contains(&role) {
                    team.insert(alias);
                }
            }
        }
    }
    Ok(out)
}

/// Every installation of `app`, in any project, with its record. An
/// unreadable record refuses: "no team" is never inferred from a file
/// the guard could not read.
pub fn installs(pm_dir: &Path, app: &str) -> Result<Vec<(String, app::Record)>> {
    let mut out = Vec::new();
    for (project_key, name) in installed_apps(pm_dir)? {
        if name != app {
            continue;
        }
        out.push((
            project_key.clone(),
            app::read_record(pm_dir, &project_key, &name)?,
        ));
    }
    Ok(out)
}

/// The agents on `app`'s team in any project.
pub fn team_agents(
    pm_dir: &Path,
    app: &str,
    store: Option<&crate::store::Store>,
) -> Result<BTreeSet<String>> {
    Ok(teams(pm_dir, store)?.remove(app).unwrap_or_default())
}

/// The apps, in any project, whose team names `agent`.
pub fn apps_of_agent(
    pm_dir: &Path,
    agent: &str,
    store: Option<&crate::store::Store>,
) -> Result<BTreeSet<String>> {
    Ok(teams(pm_dir, store)?
        .into_iter()
        .filter(|(_, team)| team.contains(agent))
        .map(|(app, _)| app)
        .collect())
}

/// The plain refusal when `author` is on `app`'s team: names the agent,
/// the app, the project and the change, and says who takes it instead.
pub fn refusal(
    pm_dir: &Path,
    app: &str,
    touched: &str,
    author: &str,
    store: Option<&crate::store::Store>,
) -> Result<Option<String>> {
    if !team_agents(pm_dir, app, store)?.contains(author) {
        return Ok(None);
    }
    Ok(Some(author_reason(pm_dir, app, touched, author)?))
}

fn author_reason(pm_dir: &Path, app: &str, touched: &str, author: &str) -> Result<String> {
    Ok(format!(
        "{author} is on app '{app}'s team ({}) and cannot author a change to that app's \
         source ({touched}) — app changes are ordinary work for the master or any agent \
         not on the app's team (plan → branch → independent review → merge → \
         `cadence app update`), never for the app's own team",
        projects_of(pm_dir, app)?
    ))
}

/// The plain refusal when `reviewer` is on `app`'s team: a reviewer
/// independent of the change, and of the app's own team, files the
/// verdict.
pub fn review_refusal(
    pm_dir: &Path,
    app: &str,
    reviewer: &str,
    store: Option<&crate::store::Store>,
) -> Result<Option<String>> {
    if !team_agents(pm_dir, app, store)?.contains(reviewer) {
        return Ok(None);
    }
    Ok(Some(format!(
        "{reviewer} is on app '{app}'s team ({}) and cannot review a change to that app's \
         source ({}/{app}/) — the review goes to an agent independent of the app's own team",
        projects_of(pm_dir, app)?,
        app::DIR
    )))
}

fn projects_of(pm_dir: &Path, app: &str) -> Result<String> {
    Ok(installs(pm_dir, app)?
        .into_iter()
        .map(|(project, _)| project)
        .collect::<Vec<_>>()
        .join(", "))
}

/// [`refusal`] for any of `apps`, each named by its `apps/<name>/`
/// source path. The first app whose team names an author answers; a
/// clean change answers `None`.
pub fn refusals(
    pm_dir: &Path,
    apps: &BTreeSet<String>,
    authors: &[&str],
    store: Option<&crate::store::Store>,
) -> Result<Option<String>> {
    if apps.is_empty() {
        return Ok(None);
    }
    let teams = teams(pm_dir, store)?;
    for app in apps {
        let Some(team) = teams.get(app) else {
            continue;
        };
        let Some(author) = authors
            .iter()
            .find(|a| !a.is_empty() && team.contains(**a))
        else {
            continue;
        };
        return Ok(Some(author_reason(
            pm_dir,
            app,
            &format!("{}/{app}/", app::DIR),
            author,
        )?));
    }
    Ok(None)
}

/// The app names `pr`'s repository is the git source of — an install
/// pinned to a git URL whose repository is this one. Empty when no
/// installation takes its source from `slug`.
pub fn apps_from_git_source(pm_dir: &Path, slug: &str) -> Result<BTreeSet<String>> {
    let want = project::normalize_remote(&format!("github.com/{slug}")).to_ascii_lowercase();
    let mut out = BTreeSet::new();
    for (project_key, name) in installed_apps(pm_dir)? {
        let record = app::read_record(pm_dir, &project_key, &name)?;
        if let app::Source::Git { url, .. } = &record.source {
            if project::normalize_remote(url).to_ascii_lowercase() == want {
                out.insert(name);
            }
        }
    }
    Ok(out)
}

/// A refusal-shaped error for the caller that returns `Result`.
pub fn refuse(why: String) -> Error {
    Error::rejected(why)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::Pm;

    fn tag(name: &str) -> String {
        name.to_string()
    }

    #[test]
    fn cad622_app_in_path_reads_only_the_apps_prefix() {
        assert_eq!(app_in_path("apps/social-content/app.md").as_deref(), Some("social-content"));
        assert_eq!(app_in_path("./apps/blog-post/").as_deref(), Some("blog-post"));
        assert_eq!(app_in_path("apps/blog-post").as_deref(), Some("blog-post"));
        assert_eq!(app_in_path("/apps/crm/workflows/x.md").as_deref(), Some("crm"));
        // Not an app path: another `apps/` depth, a sibling, an escape,
        // a name that is not tag-shaped.
        assert_eq!(app_in_path("src/apps/x/app.md"), None);
        assert_eq!(app_in_path("appstore/x"), None);
        assert_eq!(app_in_path("apps/../etc/passwd"), None);
        assert_eq!(app_in_path("apps/Upper/app.md"), None);
        assert_eq!(app_in_path("apps/"), None);
    }

    #[test]
    fn cad622_apps_in_text_needs_the_exact_source_spelling() {
        let text = "Touch `apps/social-content/app.md` and apps/blog-post/workflows/post.md. \
                    Not apps/, not src/apps/x/, not apps/Upper/, and apps/crm is a bare name.";
        let apps = apps_in_text(text);
        assert_eq!(apps, [tag("blog-post"), tag("social-content")].into_iter().collect());
    }

    /// A tracker with two projects and one app installed in each, with
    /// the saved team the operator set.
    fn tracker() -> (tempfile::TempDir, Pm) {
        let dir = tempfile::Builder::new().prefix("c622").tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        for (key, prefix) in [("demo", "DEM"), ("other", "OTH")] {
            crate::issue::write::project_add(&pm, key, prefix, &[], &[], &[], None).unwrap();
        }
        (dir, pm)
    }

    fn install(pm: &Pm, project: &str, name: &str, team: &[(&str, &str)]) {
        let dir = pm.dir.join(project).join(app::DIR);
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(
            dir.join(name).join(app::MANIFEST),
            format!("---\napp: {name}\ntitle: T\nversion: '1'\nneeds:\n  connections: []\n---\n\nGuide\n"),
        )
        .unwrap();
        let record = app::Record {
            schema: 1,
            app: name.to_string(),
            install_id: format!("{project}-{name}"),
            source: app::Source::Path {
                path: format!("tests/fixtures/apps/{name}"),
            },
            bindings: Default::default(),
            team: team
                .iter()
                .map(|(role, alias)| (role.to_string(), alias.to_string()))
                .collect(),
            installed_at: "2026-01-01T00:00:00Z".into(),
            installed_by: "operator".into(),
            updated_at: None,
            removed: None,
            restore_after: None,
            purge_after: None,
            catalogue: None,
        };
        std::fs::write(
            dir.join(format!("{name}.yaml")),
            serde_yaml::to_string(&record).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn cad622_a_team_agent_is_refused_and_another_agent_is_allowed() {
        let (_root, pm) = tracker();
        install(&pm, "demo", "social-content", &[("writer", "w1")]);
        let apps = apps_in_paths(["apps/social-content/app.md"]);
        let why = refusals(&pm.dir, &apps, &["w1"], None).unwrap().unwrap();
        assert!(why.contains("w1 is on app 'social-content's team (demo)"), "{why}");
        assert!(why.contains("apps/social-content/"), "{why}");
        assert!(why.contains("cannot author"), "{why}");
        // A positive control: the guard never refuses everyone.
        assert_eq!(refusals(&pm.dir, &apps, &["master", "w2"], None).unwrap(), None);
    }

    #[test]
    fn cad622_renaming_the_role_does_not_launder_the_author() {
        let (_root, pm) = tracker();
        // The same alias under a role key nobody would recognise: the
        // guard reads the aliases, so the key is irrelevant.
        install(&pm, "demo", "social-content", &[("author", "w1")]);
        let apps = apps_in_paths(["apps/social-content/workflows/post.md"]);
        let why = refusals(&pm.dir, &apps, &["w1"], None).unwrap();
        assert!(why.is_some(), "a renamed role must not hide the agent");
        // And a role key that looks like another agent does not bar it.
        install(&pm, "other", "blog-post", &[("w1", "w9")]);
        let apps = apps_in_paths(["apps/blog-post/app.md"]);
        assert_eq!(refusals(&pm.dir, &apps, &["w1"], None).unwrap(), None);
    }

    #[test]
    fn cad622_another_project_cannot_hide_the_team() {
        let (_root, pm) = tracker();
        // The team lives with the install in `demo`; the change is a
        // ticket of `other` (paths are repo-relative, project-free).
        install(&pm, "demo", "social-content", &[("writer", "w1")]);
        let apps = apps_in_paths(["apps/social-content/app.md"]);
        let why = refusals(&pm.dir, &apps, &["w1"], None).unwrap().unwrap();
        assert!(why.contains("(demo)"), "{why}");
        assert_eq!(apps_of_agent(&pm.dir, "w1", None).unwrap().len(), 1);
        assert!(apps_of_agent(&pm.dir, "w2", None).unwrap().is_empty());
    }

    #[test]
    fn cad622_an_unreadable_install_record_refuses() {
        let (_root, pm) = tracker();
        install(&pm, "demo", "social-content", &[("writer", "w1")]);
        std::fs::write(
            pm.dir.join("demo").join(app::DIR).join("social-content.yaml"),
            "not: a record\n",
        )
        .unwrap();
        let apps = apps_in_paths(["apps/social-content/app.md"]);
        assert!(
            refusals(&pm.dir, &apps, &["w2"], None).is_err(),
            "an unreadable record is never read as no team"
        );
    }
}
