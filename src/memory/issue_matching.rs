//! Issue-scoped match context and dispatch: which paths an issue's
//! recorded commits touched, which repo-relative paths it *declares* it
//! plans to touch, the `MatchCtx` built from its frontmatter, and the
//! accepted memories applying to it. The bounded-git helper, report-mode
//! loading and the matching/freshness policy stay in the parent and
//! `matching` — `stale` also calls `git_bounded`.

use std::path::Path;
use std::time::Duration;

use crate::error::Result;
use crate::issue::{areas, board, history, project, Pm};

use super::matching::match_memories_planned;
use super::{git_bounded, load_project_report, Freshness, MatchCtx, Matched};

/// Paths the issue's recorded code commits touched — the path-scope
/// input for issue matching. Absent commits/repos simply yield none.
fn issue_paths(pm_dir: &Path, issue: &board::Issue) -> Vec<String> {
    let mut paths = Vec::new();
    let (commits, _) = history::code_commits(pm_dir, issue);
    for c in commits {
        let (Some(repo), Some(sha)) = (c["repo"].as_str(), c["sha"].as_str()) else {
            continue;
        };
        let dir = project::expand_home(repo);
        let out = git_bounded(
            &dir,
            &["show", "--format=", "--name-only", sha],
            Duration::from_secs(5),
        )
        .unwrap_or_default();
        paths.extend(out.lines().map(str::to_string));
    }
    // Explicit commit refs too — a ref path is a sha in a project repo.
    for r in issue.front.refs.iter().filter(|r| r.kind == "commit") {
        let Some(sha) = r.path.as_deref() else {
            continue;
        };
        let Ok(projects) = project::list(pm_dir) else {
            break;
        };
        for p in projects.iter().filter(|p| p.key == issue.project) {
            for repo in &p.repos {
                let Some(path) = &repo.path else { continue };
                let dir = project::expand_home(path);
                if git_bounded(
                    &dir,
                    &["cat-file", "-e", &format!("{sha}^{{commit}}")],
                    Duration::from_secs(5),
                )
                .is_err()
                {
                    continue;
                }
                if let Ok(out) = git_bounded(
                    &dir,
                    &["show", "--format=", "--name-only", sha],
                    Duration::from_secs(5),
                ) {
                    paths.extend(out.lines().map(str::to_string));
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// The issue's declared planned paths (`paths:` frontmatter, CAD-378) —
/// the path scope that exists before any commit does. Frontmatter is
/// agent-writable and parsed leniently, so every declaration is re-checked
/// against the native path grammar: an absolute path, a `..` segment, a
/// control byte or an over-budget pattern selects nothing rather than
/// reaching outside the repo. Sorted, de-duplicated and cut at the native
/// budget so the same file always yields the same selection.
///
/// This is both the selection input and what `memory match --issue
/// --json` reports as `context.planned_paths`, so the diagnostic can
/// never disagree with what matching actually used.
pub fn issue_planned_paths(issue: &board::Issue) -> Vec<String> {
    let mut paths: Vec<String> = issue
        .front
        .paths
        .iter()
        .filter(|decl| areas::check_path(decl).is_ok())
        .cloned()
        .collect();
    paths.sort();
    paths.dedup();
    paths.truncate(areas::PATHS_MAX);
    paths
}

/// Match context for an issue: component, frontmatter tags,
/// recorded-commit paths, and the target worker's provider.
pub fn issue_ctx(
    pm: &Pm,
    issue: &board::Issue,
    provider: Option<&str>,
    role: Option<&str>,
) -> Result<MatchCtx> {
    Ok(MatchCtx {
        components: issue.front.component.clone().into_iter().collect(),
        paths: issue_paths(&pm.dir, issue),
        providers: provider.map(|p| vec![p.to_string()]).unwrap_or_default(),
        tags: issue.front.tags.clone(),
        roles: role.map(|role| vec![role.to_owned()]).unwrap_or_default(),
    })
}

/// `match_for_issue` plus the context it matched with, and room for the
/// caller to add its own axes: the `memory match --issue` diagnostic
/// layers `--component/--path/--tag` here, so its selection is exactly
/// what delivery selects plus what the operator asked for — never a
/// second policy.
pub fn match_for_issue_ctx(
    pm: &Pm,
    issue: &board::Issue,
    provider: Option<&str>,
    role: Option<&str>,
    extend: impl FnOnce(&mut MatchCtx),
) -> Result<(Matched, MatchCtx, Vec<String>)> {
    let mut ctx = issue_ctx(pm, issue, provider, role)?;
    extend(&mut ctx);
    let planned = issue_planned_paths(issue);
    let (pool, errors) = load_project_report(&pm.dir, &issue.project);
    let fresh = Freshness::for_key(&pm.dir, &issue.project);
    Ok((
        match_memories_planned(&pool, &ctx, &fresh, &planned),
        ctx,
        errors,
    ))
}

/// The dispatch/match surface: accepted memories applying to an
/// issue — its recorded-commit paths, components, tags, provider and
/// role, plus the planned paths it declares, so a first-task dispatch is
/// guided before any commit exists. Report-mode load — valid records
/// still match when sibling files are broken; the caller surfaces
/// `errors`.
pub fn match_for_issue(
    pm: &Pm,
    issue: &board::Issue,
    provider: Option<&str>,
    role: Option<&str>,
) -> Result<(Matched, Vec<String>)> {
    let (matched, _, errors) = match_for_issue_ctx(pm, issue, provider, role, |_| {})?;
    Ok((matched, errors))
}
