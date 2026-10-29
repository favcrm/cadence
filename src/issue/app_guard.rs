//! CAD-622: an app's own team agents cannot author changes to that app.
//!
//! Operator decision (2026-09-26): app changes are ordinary work — the
//! master or any connected agent may author them (plan → branch →
//! independent review → merge → `cadence app update` → operator
//! re-approval for structural changes). There is no dedicated "Improve
//! this app" feature. The one exception is the app's own team: an agent
//! the operator assigned to an installation's team never authors a
//! change to that app's source, and never reviews one.
//!
//! **What a change touches.** A repo-relative path under `apps/<name>/`
//! (a ticket's planned `paths:`, a plan naming the path, a PR's changed
//! files), or a repository an installation pinned as the app's git
//! source — `app.md` and `workflows/…` at that repository's root need no
//! `apps/<name>/` directory.
//!
//! **Who is on the team**, from every installation of that app name in
//! any project — membership is by agent alias, never by a role key, so
//! renaming a role cannot launder an author, and a ticket in another
//! project cannot either:
//!
//! - the saved team roles of a legacy project install
//!   (`<pm>/<project>/apps/<name>.yaml` → `team`);
//! - the saved team of a workspace installation
//!   (`.apps/installations/<id>/record.yaml` + the daemon's
//!   `app-teams.json`, read through the same [`team_of`] the app-run
//!   RPC uses);
//! - with a store, each installation's frozen run role inputs and the
//!   agents its runs assigned — the run snapshot, never the current
//!   workflow.
//!
//! **Where it is enforced.** The daemon's own dispatch admission
//! (`dispatch_send`) with the connection-derived caller and the target
//! worker; `plan_propose`, including a grant's rendered plan before any
//! admission side effect; the review loop (the PR's own changed files
//! plus known git sources), where unavailable evidence holds the record
//! instead of passing it; review routing and the verdict.
//!
//! Fail closed: an install record, a team store or a run snapshot that
//! cannot be read refuses — it is never read as "no team".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::issue::{app, model, project};

/// The three inputs every gate needs: the tracker, the daemon state dir
/// (the workspace team store) and, where one is open, the store (run
/// membership). A CLI seam passes what it has; the daemon passes all.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub pm_dir: &'a Path,
    pub state_dir: Option<&'a Path>,
    pub store: Option<&'a crate::store::Store>,
}

impl<'a> Ctx<'a> {
    /// The tracker alone — no workspace team store, no run membership.
    pub fn tracker(pm_dir: &'a Path) -> Self {
        Self {
            pm_dir,
            state_dir: None,
            store: None,
        }
    }
}

/// What the guard knows about a change: nothing to check, the apps it
/// conclusively touches, or evidence it could not read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// No author or reviewer is on any app's team and no app takes its
    /// source from this repository — the question did not arise.
    NotApplicable,
    Known {
        apps: BTreeSet<String>,
        /// The apps among them recognised by their pinned git source.
        from_git: BTreeSet<String>,
    },
    Unavailable(String),
}

/// One installation of an app: where it lives, where its bytes come
/// from, and the agents on its team.
#[derive(Clone, Debug)]
pub struct Install {
    pub app: String,
    pub project: Option<String>,
    pub install_id: String,
    /// The normalized remote of a `Source::Git` install, if any.
    pub git: Option<String>,
    pub team: BTreeSet<String>,
}

/// The app a repo-relative path belongs to: `apps/<name>[/…]` → `<name>`.
/// Leading `./` and `/` segments are dropped; a deeper `apps/` component
/// (`src/apps/x/`), a `..` escape or a name that is not tag-shaped is
/// not an app path.
pub fn app_in_path(path: &str) -> Option<String> {
    let mut path = path.trim();
    loop {
        let next = path
            .strip_prefix("./")
            .or_else(|| path.strip_prefix('/'))
            .unwrap_or(path);
        if next == path {
            break;
        }
        path = next;
    }
    let mut parts = path.split('/');
    if parts.next() != Some(app::DIR) {
        return None;
    }
    let name = parts.next()?;
    model::valid_tag(name).then(|| name.to_string())
}

/// Every app named by `paths` — a ticket's declared files, or a PR's
/// changed files.
pub fn apps_in_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    paths.into_iter().filter_map(app_in_path).collect()
}

/// Every app a plan's prose names as a source path. A plan ticket has no
/// `paths:` field, so the path it writes is the evidence it touches the
/// app. Tokens are normalized first, so `./apps/<name>/` counts exactly
/// like `apps/<name>/`, while a genuinely nested `src/apps/<name>/`
/// stays out.
pub fn apps_in_text(text: &str) -> BTreeSet<String> {
    text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':' | '<' | '>'
            )
    })
    .filter_map(|token| {
        let token = token.trim_matches(|c: char| {
            !(c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_'))
        });
        (!token.is_empty()).then_some(token).and_then(app_in_path)
    })
    .collect()
}

/// `(project, app)` for every legacy install record in the tracker.
fn legacy_apps(pm_dir: &Path) -> Result<Vec<(String, String)>> {
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
            if let Some(stem) = name.strip_suffix(".yaml") {
                if model::valid_tag(stem) {
                    out.push((p.key.clone(), stem.to_string()));
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

/// `.apps/installations/<id>/record.yaml` — the workspace registry. A
/// directory without a readable record is not an installation (the
/// catalog's own rule); a record that will not parse refuses.
fn workspace_records(pm_dir: &Path) -> Result<Vec<(String, app::Record)>> {
    let root = pm_dir.join(".apps").join("installations");
    if root.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Ok(Vec::new());
    }
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().to_string();
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        crate::issue::app_catalog::InstallationId::parse(&id)?;
        dirs.push((id, entry.path()));
    }
    dirs.sort();
    for (id, dir) in dirs {
        let file = dir.join("record.yaml");
        if file.symlink_metadata().is_err() {
            continue;
        }
        let text = std::fs::read_to_string(&file).map_err(|e| {
            Error::rejected(format!(
                "workspace installation {id}: its record cannot be read ({e}) — the \
                 app-team guard cannot decide who is on its team"
            ))
        })?;
        let record: app::Record = serde_yaml::from_str(&text).map_err(|e| {
            Error::rejected(format!(
                "workspace installation {id}: its record is not valid ({e})"
            ))
        })?;
        if record.install_id.is_empty() {
            let mut record = record;
            record.install_id = id;
            out.push((record.install_id.clone(), record));
        } else {
            out.push((record.install_id.clone(), record));
        }
    }
    Ok(out)
}

fn git_of(record: &app::Record) -> Option<String> {
    match &record.source {
        app::Source::Git { url, .. } => Some(project::normalize_remote(url).to_ascii_lowercase()),
        _ => None,
    }
}

/// Every installation of every app, in any project, with its resolved
/// team.
pub fn installations(ctx: Ctx) -> Result<Vec<Install>> {
    let mut out: Vec<Install> = Vec::new();
    for (project_key, name) in legacy_apps(ctx.pm_dir)? {
        let record = app::read_record(ctx.pm_dir, &project_key, &name)?;
        out.push(Install {
            app: name,
            project: Some(project_key),
            install_id: record.install_id.clone(),
            git: git_of(&record),
            team: record.team.values().cloned().collect(),
        });
    }
    for (id, record) in workspace_records(ctx.pm_dir)? {
        let mut team: BTreeSet<String> = record.team.values().cloned().collect();
        if let Some(state) = ctx.state_dir {
            if let Some(saved) = crate::daemon::app_teams_rpc::team_of(state, &id)? {
                team.extend(saved.roles.values().cloned());
            }
        }
        out.push(Install {
            app: record.app.clone(),
            // A workspace installation has no project link in its record.
            project: None,
            install_id: id,
            git: git_of(&record),
            team,
        });
    }
    if let Some(store) = ctx.store {
        for install in &mut out {
            // The roles a run input can name: the saved team's keys and
            // the roles the app's workflows declare (legacy installs
            // have a project; a workspace install's snapshot still
            // carries the agents it assigned).
            let mut roles: BTreeSet<String> = install.team.clone();
            if let Some(project_key) = install.project.as_deref() {
                roles.extend(
                    app::team_roles(ctx.pm_dir, project_key, &install.app).unwrap_or_default(),
                );
            }
            for (inputs, assigned) in store.app_run_team_inputs(&install.install_id)? {
                install.team.extend(assigned);
                for (role, alias) in inputs {
                    if roles.contains(&role) {
                        install.team.insert(alias);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Every installed app's team in one walk: app name → the agents on its
/// team in any project, from any installation.
pub fn teams(ctx: Ctx) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for install in installations(ctx)? {
        out.entry(install.app).or_default().extend(install.team);
    }
    Ok(out)
}

/// The agents on `app`'s team in any project.
pub fn team_agents(ctx: Ctx, app: &str) -> Result<BTreeSet<String>> {
    Ok(teams(ctx)?.remove(app).unwrap_or_default())
}

/// The apps, in any project, whose team names `agent`.
pub fn apps_of_agent(ctx: Ctx, agent: &str) -> Result<BTreeSet<String>> {
    Ok(teams(ctx)?
        .into_iter()
        .filter(|(_, team)| team.contains(agent))
        .map(|(app, _)| app)
        .collect())
}

/// The apps installed from the repository `slug` (`owner/name`) — the
/// app's own git source, whatever its directory layout.
pub fn apps_from_git_source(ctx: Ctx, slug: &str) -> Result<BTreeSet<String>> {
    let want = project::normalize_remote(&format!("github.com/{slug}")).to_ascii_lowercase();
    Ok(installations(ctx)?
        .into_iter()
        .filter(|i| i.git.as_deref() == Some(want.as_str()))
        .map(|i| i.app)
        .collect())
}

/// The apps installed from any of `remotes` (already normalized, or raw
/// — both are compared normalized).
pub fn apps_from_remotes(ctx: Ctx, remotes: &[String]) -> Result<BTreeSet<String>> {
    let want: BTreeSet<String> = remotes
        .iter()
        .map(|r| project::normalize_remote(r).to_ascii_lowercase())
        .filter(|r| !r.is_empty())
        .collect();
    if want.is_empty() {
        return Ok(BTreeSet::new());
    }
    Ok(installations(ctx)?
        .into_iter()
        .filter(|i| i.git.as_ref().is_some_and(|g| want.contains(g)))
        .map(|i| i.app)
        .collect())
}

/// The plain refusal when `author` is on `app`'s team: names the agent,
/// the app, where it is installed, and the change, and says who takes it
/// instead.
pub fn refusal(ctx: Ctx, app: &str, touched: &str, author: &str) -> Result<Option<String>> {
    if !team_agents(ctx, app)?.contains(author) {
        return Ok(None);
    }
    Ok(Some(author_reason(ctx, app, touched, author)?))
}

fn author_reason(ctx: Ctx, app: &str, touched: &str, author: &str) -> Result<String> {
    Ok(format!(
        "{author} is on app '{app}'s team ({}) and cannot author a change to that app's \
         source ({touched}) — app changes are ordinary work for the master or any agent \
         not on the app's team (plan → branch → independent review → merge → \
         `cadence app update`), never for the app's own team",
        where_installed(ctx, app)?
    ))
}

/// The plain refusal when `reviewer` is on `app`'s team.
pub fn review_refusal(ctx: Ctx, app: &str, reviewer: &str) -> Result<Option<String>> {
    if !team_agents(ctx, app)?.contains(reviewer) {
        return Ok(None);
    }
    Ok(Some(format!(
        "{reviewer} is on app '{app}'s team ({}) and cannot review a change to that app's \
         source — the review goes to an agent independent of the app's own team",
        where_installed(ctx, app)?
    )))
}

fn where_installed(ctx: Ctx, app: &str) -> Result<String> {
    let mut where_: Vec<String> = Vec::new();
    for install in installations(ctx)? {
        if install.app != app {
            continue;
        }
        where_.push(match &install.project {
            Some(project) => format!("project {project}"),
            None => format!("installation {}", install.install_id),
        });
    }
    where_.dedup();
    Ok(where_.join(", "))
}

/// [`refusal`] for any of `apps` plus every app installed from one of
/// `remotes`, each named by its source path. The first app whose team
/// names an author answers; a clean change answers `None`.
pub fn refusals(
    ctx: Ctx,
    apps: &BTreeSet<String>,
    authors: &[&str],
    remotes: &[String],
) -> Result<Option<String>> {
    let mut apps = apps.clone();
    apps.extend(apps_from_remotes(ctx, remotes)?);
    if apps.is_empty() {
        return Ok(None);
    }
    let teams = teams(ctx)?;
    let git: BTreeSet<String> = installations(ctx)?
        .into_iter()
        .filter_map(|i| i.git.map(|_| i.app))
        .collect();
    for app in &apps {
        let Some(team) = teams.get(app) else { continue };
        let Some(author) = authors.iter().find(|a| !a.is_empty() && team.contains(**a)) else {
            continue;
        };
        let touched = if git.contains(app) {
            "its pinned git source".to_string()
        } else {
            format!("{}/{app}/", app::DIR)
        };
        return Ok(Some(author_reason(ctx, app, &touched, author)?));
    }
    Ok(None)
}

/// A refusal-shaped error for the caller that returns `Result`.
pub fn refuse(why: String) -> Error {
    Error::rejected(why)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::Pm;

    fn tracker() -> (tempfile::TempDir, Pm) {
        let dir = tempfile::Builder::new().prefix("c622").tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        for (key, prefix) in [("demo", "DEM"), ("other", "OTH")] {
            crate::issue::write::project_add(&pm, key, prefix, &[], &[], &[], None).unwrap();
        }
        (dir, pm)
    }

    fn record(
        name: &str,
        install_id: &str,
        source: app::Source,
        team: &[(&str, &str)],
    ) -> app::Record {
        app::Record {
            schema: 1,
            app: name.to_string(),
            install_id: install_id.to_string(),
            source,
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
        }
    }

    fn install(pm: &Pm, project: &str, name: &str, team: &[(&str, &str)]) {
        let dir = pm.dir.join(project).join(app::DIR);
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(
            dir.join(name).join(app::MANIFEST),
            format!("---\napp: {name}\ntitle: T\nversion: '1'\nneeds:\n  connections: []\n---\n\nGuide\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join(format!("{name}.yaml")),
            serde_yaml::to_string(&record(
                name,
                &format!("{project}-{name}"),
                app::Source::Path { path: "x".into() },
                team,
            ))
            .unwrap(),
        )
        .unwrap();
    }

    /// A workspace installation: `.apps/installations/<id>/record.yaml`
    /// with no project, its team in the daemon's `app-teams.json`.
    fn workspace_install(pm: &Pm, state: &Path, id: &str, name: &str, roles: &[(&str, &str)]) {
        let dir = pm.dir.join(".apps").join("installations").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("record.yaml"),
            serde_yaml::to_string(&record(
                name,
                id,
                app::Source::Git {
                    url: "https://github.com/acme/app-repo".into(),
                    sha: "0".repeat(40),
                    dir: None,
                },
                &[],
            ))
            .unwrap(),
        )
        .unwrap();
        let teams = serde_json::json!({"schema": 1, "teams": {id: {
            "owner_pm": "lead",
            "roles": roles.iter().map(|(r, a)| (r.to_string(), a.to_string())).collect::<std::collections::BTreeMap<_,_>>(),
            "revision": 1,
        }}});
        std::fs::create_dir_all(state).unwrap();
        std::fs::write(state.join("app-teams.json"), teams.to_string()).unwrap();
    }

    #[test]
    fn cad622_app_in_path_reads_only_the_apps_prefix() {
        assert_eq!(
            app_in_path("apps/social-content/app.md").as_deref(),
            Some("social-content")
        );
        assert_eq!(
            app_in_path("./apps/blog-post/").as_deref(),
            Some("blog-post")
        );
        assert_eq!(app_in_path("apps/blog-post").as_deref(), Some("blog-post"));
        assert_eq!(
            app_in_path("/apps/crm/workflows/x.md").as_deref(),
            Some("crm")
        );
        assert_eq!(app_in_path("src/apps/x/app.md"), None);
        assert_eq!(app_in_path("appstore/x"), None);
        assert_eq!(app_in_path("apps/../etc/passwd"), None);
        assert_eq!(app_in_path("apps/Upper/app.md"), None);
        assert_eq!(app_in_path("apps/"), None);
    }

    #[test]
    fn cad622_apps_in_text_normalizes_the_source_spelling() {
        let text = "Touch `apps/social-content/app.md`, ./apps/blog-post/workflows/post.md \
                    and (./apps/crm/app.md). Not src/apps/x/app.md, not apps/, not apps/Upper/.";
        let apps = apps_in_text(text);
        let want: BTreeSet<String> = ["blog-post", "crm", "social-content"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert_eq!(apps, want);
    }

    #[test]
    fn cad622_a_team_agent_is_refused_and_another_agent_is_allowed() {
        let (_root, pm) = tracker();
        install(&pm, "demo", "social-content", &[("writer", "w1")]);
        let ctx = Ctx::tracker(&pm.dir);
        let apps = apps_in_paths(["apps/social-content/app.md"]);
        let why = refusals(ctx, &apps, &["w1"], &[]).unwrap().unwrap();
        assert!(why.contains("w1 is on app 'social-content's team"), "{why}");
        assert!(why.contains("project demo"), "{why}");
        assert_eq!(refusals(ctx, &apps, &["master", "w2"], &[]).unwrap(), None);
    }

    #[test]
    fn cad622_a_workspace_installation_team_is_read_from_the_daemon_store() {
        let (root, pm) = tracker();
        let state = root.path().join("state");
        workspace_install(
            &pm,
            &state,
            "inst-w1",
            "social-content",
            &[("writer", "w1")],
        );
        let ctx = Ctx {
            pm_dir: &pm.dir,
            state_dir: Some(&state),
            store: None,
        };
        // The saved team is not in the record — only in app-teams.json.
        assert!(teams(ctx).unwrap()["social-content"].contains("w1"));
        let apps = apps_in_paths(["apps/social-content/app.md"]);
        assert!(refusals(ctx, &apps, &["w1"], &[]).unwrap().is_some());
        // And the pinned git source is recognised with no apps/<name>/ dir.
        assert_eq!(
            apps_from_git_source(ctx, "acme/app-repo").unwrap(),
            ["social-content".to_string()].into_iter().collect()
        );
        assert!(refusals(
            ctx,
            &BTreeSet::new(),
            &["w1"],
            &["github.com/acme/app-repo".into()]
        )
        .unwrap()
        .is_some());
        // Without the state dir the saved team is not visible — the
        // seam that lacks the store says so by refusing nothing.
        let blind = Ctx::tracker(&pm.dir);
        assert!(teams(blind).unwrap()["social-content"].is_empty());
    }

    #[test]
    fn cad622_renaming_the_role_does_not_launder_the_author() {
        let (_root, pm) = tracker();
        install(&pm, "demo", "social-content", &[("author", "w1")]);
        let ctx = Ctx::tracker(&pm.dir);
        let apps = apps_in_paths(["apps/social-content/workflows/post.md"]);
        assert!(refusals(ctx, &apps, &["w1"], &[]).unwrap().is_some());
        install(&pm, "other", "blog-post", &[("w1", "w9")]);
        let apps = apps_in_paths(["apps/blog-post/app.md"]);
        assert_eq!(refusals(ctx, &apps, &["w1"], &[]).unwrap(), None);
    }

    #[test]
    fn cad622_another_project_cannot_hide_the_team() {
        let (_root, pm) = tracker();
        install(&pm, "demo", "social-content", &[("writer", "w1")]);
        let ctx = Ctx::tracker(&pm.dir);
        let apps = apps_in_paths(["apps/social-content/app.md"]);
        let why = refusals(ctx, &apps, &["w1"], &[]).unwrap().unwrap();
        assert!(why.contains("project demo"), "{why}");
        assert_eq!(apps_of_agent(ctx, "w1").unwrap().len(), 1);
        assert!(apps_of_agent(ctx, "w2").unwrap().is_empty());
    }

    #[test]
    fn cad622_an_unreadable_install_record_refuses() {
        let (_root, pm) = tracker();
        install(&pm, "demo", "social-content", &[("writer", "w1")]);
        std::fs::write(
            pm.dir
                .join("demo")
                .join(app::DIR)
                .join("social-content.yaml"),
            "not: a record\n",
        )
        .unwrap();
        let ctx = Ctx::tracker(&pm.dir);
        let apps = apps_in_paths(["apps/social-content/app.md"]);
        assert!(refusals(ctx, &apps, &["w2"], &[]).is_err());
    }
}
