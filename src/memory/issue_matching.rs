//! Issue-scoped match context and dispatch: which paths an issue's
//! recorded commits touched, the `MatchCtx` built from its frontmatter,
//! and the accepted memories applying to it. The bounded-git helper,
//! report-mode loading and the matching/freshness policy stay in the
//! parent and `matching` — `stale` also calls `git_bounded`.

use std::path::Path;
use std::time::Duration;

use crate::error::Result;
use crate::issue::{board, history, project, Pm};

use super::{git_bounded, load_project_report, match_memories, Freshness, MatchCtx, Matched};

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

/// Match context for an issue: component, frontmatter tags,
/// recorded-commit paths, and the target worker's provider.
pub fn issue_ctx(pm: &Pm, issue: &board::Issue, provider: Option<&str>) -> Result<MatchCtx> {
    Ok(MatchCtx {
        components: issue.front.component.clone().into_iter().collect(),
        paths: issue_paths(&pm.dir, issue),
        providers: provider.map(|p| vec![p.to_string()]).unwrap_or_default(),
        tags: issue.front.tags.clone(),
    })
}

/// The dispatch/match surface: accepted memories applying to an
/// issue. Report-mode load — valid records still match when sibling
/// files are broken; the caller surfaces `errors`.
pub fn match_for_issue(
    pm: &Pm,
    issue: &board::Issue,
    provider: Option<&str>,
) -> Result<(Matched, Vec<String>)> {
    let ctx = issue_ctx(pm, issue, provider)?;
    let (pool, errors) = load_project_report(&pm.dir, &issue.project);
    let fresh = Freshness::for_key(&pm.dir, &issue.project);
    Ok((match_memories(&pool, &ctx, &fresh), errors))
}
