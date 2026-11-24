//! Repository discovery and integration base ref verification helpers
//! for `issue start`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::issue::{finish, git, project};
use crate::worktree;

/// The project's declared repo roots, canonicalized and de-duplicated:
/// two rows naming one checkout are one repo (CAD-2001).
pub fn declared_repos(project: &project::Project) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for path in project.repos.iter().filter_map(|r| r.path.as_deref()) {
        let expanded = project::expand_home(path);
        let root = expanded.canonicalize().unwrap_or(expanded);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

fn repo_list(project: &project::Project, candidates: &[PathBuf]) -> String {
    if candidates.is_empty() {
        format!("{} declares no repos with a local path", project.key)
    } else {
        format!(
            "{} declares: {}",
            project.key,
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Repo resolution per decision 1: explicit `--repo` (which must be a
/// declared repo — undeclared work never surfaces in code-commit
/// discovery), then the cwd's repo when it is one of the project's,
/// then the project's only repo, else refuse naming the candidates.
/// Returns the main checkout root (linked worktrees resolve to it).
pub(crate) fn resolve_repo(
    project: &project::Project,
    flag: Option<&Path>,
    cwd: &Path,
) -> Result<PathBuf> {
    let candidates = declared_repos(project);
    if let Some(dir) = flag {
        let root = worktree::main_root(dir)?;
        if !candidates.contains(&root) {
            return Err(Error::rejected(format!(
                "Repo {} is not declared — {}. Add it to `repos` in \
                 project.yaml first",
                root.display(),
                repo_list(project, &candidates)
            )));
        }
        return Ok(root);
    }
    if let Some((root, _)) = project::repo_identity(cwd) {
        if candidates.contains(&root) {
            return Ok(root);
        }
    }
    if candidates.len() == 1 {
        return worktree::main_root(&candidates[0]);
    }
    if candidates.is_empty() {
        // Nothing to pass `--repo` with: the project registers no local
        // checkout at all, so say that instead of asking for a choice.
        return Err(Error::rejected(format!(
            "Project {} declares no repo with a local path — register one in `repos` \
             in project.yaml before starting work",
            project.key
        )));
    }
    Err(Error::rejected(format!(
        "Which repo does this issue work in? Pass --repo — {}",
        repo_list(project, &candidates)
    )))
}

/// The ref and commit a new lane is cut from.
#[derive(Debug)]
pub(crate) struct ResolvedBase {
    pub name: String,
    pub sha: String,
    /// Set when the main checkout's tip is not contained in `sha`.
    /// The lane is still cut from `sha`; this only names the mismatch.
    pub warning: Option<String>,
}

/// The integration branch configured on the project repo whose checkout
/// is `root`. `None` when no row for that checkout configures one. Two
/// rows for the same checkout that name different bases are refused: the
/// lane base must never depend on which row is read first (CAD-2001).
pub(crate) fn configured_integration<'a>(
    project: &'a project::Project,
    root: &Path,
) -> Result<Option<&'a str>> {
    let Ok(root) = root.canonicalize() else {
        return Ok(None);
    };
    let mut bases: Vec<&'a str> = Vec::new();
    for repo in &project.repos {
        let Some(path) = repo.path.as_deref() else {
            continue;
        };
        let expanded = project::expand_home(path);
        if expanded.canonicalize().ok().as_deref() != Some(root.as_path()) {
            continue;
        }
        let Some(base) = repo.base.as_deref() else {
            continue;
        };
        if !bases.contains(&base) {
            bases.push(base);
        }
    }
    match bases.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        many => Err(Error::rejected(format!(
            "{} is recorded {} times in project {}'s repos with different bases ({}) — \
             keep one repos[] row per repo",
            root.display(),
            many.len(),
            project.key,
            many.join(", ")
        ))),
    }
}

/// Base resolution for a new lane.
///
/// An explicit `--base` is used as given, except that a branch name `origin`
/// carries resolves to the remote's current tip (see
/// [`verify_explicit_base`]). Otherwise the base is the
/// project's integration ref, fetched from the remote: `configured`
/// (`repos[].base`), else `origin/HEAD` when that symbolic ref exists,
/// else the remote's symbolic HEAD (`git ls-remote --symref origin
/// HEAD`). A repository with no `origin` remote still uses its current
/// branch — there is no remote integration ref to adopt instead.
///
/// When `origin` exists but its integration branch cannot be resolved,
/// this refuses. It does not adopt the main checkout's current branch.
/// When the checkout's tip is not contained in the integration commit,
/// the lane is still cut from that commit and [`ResolvedBase::warning`]
/// names the mismatch.
pub(crate) fn resolve_base(
    root: &Path,
    flag: Option<&str>,
    configured: Option<&str>,
) -> Result<ResolvedBase> {
    if let Some(base) = flag {
        return Ok(ResolvedBase {
            sha: verify_explicit_base(root, base)?,
            name: base.to_string(),
            warning: None,
        });
    }
    if let Some(configured) = configured {
        return pin_integration(root, normalize_integration(configured)?);
    }
    if let Ok(origin_head) = git(
        root,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    ) {
        return pin_integration(root, origin_head);
    }
    if origin_configured(root) {
        let branch = remote_head_branch(root)?;
        return pin_integration(root, format!("origin/{branch}"));
    }
    let base =
        git(root, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_else(|_| "HEAD".to_string());
    Ok(ResolvedBase {
        sha: verify_base(root, &base)?,
        name: base,
        warning: None,
    })
}

fn pin_integration(root: &Path, base: String) -> Result<ResolvedBase> {
    fetch_integration(root, &base)?;
    let sha = verify_base(root, &base)?;
    let warning = checkout_divergence(root, &base, &sha);
    Ok(ResolvedBase {
        name: base,
        sha,
        warning,
    })
}

/// An explicit `--base` naming a branch that exists on `origin` resolves to
/// the remote's tip now, not to a local ref that may lag it. An unreachable
/// origin or a refused fetch refuses the start. A full commit SHA, a ref
/// origin does not carry as a branch (a local branch, a tag), and a
/// repository with no `origin` keep resolving locally.
fn verify_explicit_base(root: &Path, base: &str) -> Result<String> {
    let is_sha = matches!(base.len(), 40 | 64) && base.chars().all(|c| c.is_ascii_hexdigit());
    if is_sha || !origin_configured(root) {
        return verify_base(root, base);
    }
    let remote_form = base
        .strip_prefix("refs/remotes/origin/")
        .or_else(|| base.strip_prefix("origin/"));
    let spelled = remote_form
        .or_else(|| base.strip_prefix("refs/heads/"))
        .unwrap_or(base);
    let branch = if remote_form == Some("HEAD") {
        remote_head_branch(root)?
    } else {
        spelled.to_string()
    };
    if !valid_branch(&branch) {
        return verify_base(root, base);
    }
    let want = format!("refs/heads/{branch}");
    let listed = git(root, &["ls-remote", "--heads", "origin", &want]).map_err(|e| {
        Error::rejected(format!(
            "Could not reach origin to resolve base '{base}' in {} ({e}) — retry when origin is \
             reachable; no stale local ref was used",
            root.display()
        ))
    })?;
    let on_origin = listed
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some(want.as_str()));
    if !on_origin {
        if remote_form.is_some() {
            return Err(Error::rejected(format!(
                "Base '{base}' names a branch origin no longer carries — pass --base <ref> \
                 that exists on origin; the stale tracking ref was not used"
            )));
        }
        return verify_base(root, base);
    }
    fetch_integration(root, &format!("origin/{branch}"))?;
    // The full ref: a local tag or branch named `origin/<branch>` would win
    // the short spelling.
    verify_base(root, &format!("refs/remotes/origin/{branch}"))
}

fn verify_base(root: &Path, base: &str) -> Result<String> {
    git(
        root,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )
    .map_err(|_| {
        Error::rejected(format!(
            "Base '{base}' does not resolve in {} — pass --base <ref>",
            root.display()
        ))
    })
}

fn origin_configured(root: &Path) -> bool {
    git(root, &["config", "--get", "remote.origin.url"])
        .ok()
        .is_some_and(|url| !url.is_empty())
}

fn remote_head_branch(root: &Path) -> Result<String> {
    let label = checkout_label(root);
    let out = git(root, &["ls-remote", "--symref", "origin", "HEAD"]).map_err(|e| {
        Error::rejected(format!(
            "Refusing to adopt the main checkout's branch '{label}' as the lane base in {}: \
             origin has no resolvable integration branch ({e}). Pass --base <ref>, or set \
             repos[].base to the integration branch",
            root.display()
        ))
    })?;
    let branch = out.lines().find_map(|line| {
        let rest = line.strip_prefix("ref:")?.trim_start();
        let name = rest.split_whitespace().next()?;
        name.strip_prefix("refs/heads/")
    });
    match branch {
        Some(branch) if valid_branch(branch) => Ok(branch.to_string()),
        _ => Err(Error::rejected(format!(
            "Refusing to adopt the main checkout's branch '{label}' as the lane base in {}: \
             origin HEAD is not a branch. Pass --base <ref>, or set repos[].base to the \
             integration branch",
            root.display()
        ))),
    }
}

fn fetch_integration(root: &Path, base: &str) -> Result<()> {
    let (remote, branch) = base.split_once('/').ok_or_else(|| {
        Error::rejected(format!(
            "Integration ref '{base}' is not a remote-tracking name — expected 'origin/<branch>'"
        ))
    })?;
    if !valid_remote(remote) || !valid_branch(branch) {
        return Err(Error::rejected(format!(
            "Integration ref '{base}' is not a branch on origin"
        )));
    }
    let refspec = format!("+refs/heads/{branch}:refs/remotes/{remote}/{branch}");
    git(
        root,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            remote,
            &refspec,
        ],
    )
    .map(|_| ())
    .map_err(|e| {
        Error::rejected(format!(
            "Could not refresh integration ref '{base}' in {} ({e}) — retry when origin is \
             reachable or pass --base <ref>",
            root.display()
        ))
    })
}

/// `staging` and `origin/staging` both mean `origin/staging`.
fn normalize_integration(spec: &str) -> Result<String> {
    let spec = spec.trim();
    let branch = spec
        .strip_prefix("refs/remotes/origin/")
        .or_else(|| spec.strip_prefix("refs/heads/"))
        .or_else(|| spec.strip_prefix("origin/"))
        .unwrap_or(spec);
    if !valid_branch(branch) {
        return Err(Error::rejected(format!(
            "Integration ref '{spec}' is not a branch name — set repos[].base to a branch such \
             as 'staging' or 'origin/staging'"
        )));
    }
    Ok(format!("origin/{branch}"))
}

fn valid_remote(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn valid_branch(branch: &str) -> bool {
    branch != "HEAD"
        && !branch.is_empty()
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && !branch.contains("//")
        && branch.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

fn checkout_label(root: &Path) -> String {
    let raw =
        git(root, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_else(|_| "HEAD".to_string());
    raw.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

fn short_sha(sha: &str) -> &str {
    let n = sha.len().min(12);
    &sha[..n]
}

/// `None` when the main checkout's tip is contained in the integration
/// commit (equal, or behind it). A foreign tip is named, not adopted.
fn checkout_divergence(root: &Path, base: &str, base_sha: &str) -> Option<String> {
    let sha = git(root, &["rev-parse", "--verify", "HEAD^{commit}"]).ok()?;
    if git(root, &["merge-base", "--is-ancestor", &sha, base_sha]).is_ok() {
        return None;
    }
    Some(format!(
        "main checkout '{}' ({}) is not contained in integration ref {base} ({}); the lane is \
         based on {base}, not that checkout",
        checkout_label(root),
        short_sha(&sha),
        short_sha(base_sha),
    ))
}

pub(super) fn branch_base(root: &Path, branch: &str, fallback: &str) -> String {
    git(
        root,
        &[
            "reflog",
            "show",
            "--format=%H",
            &format!("refs/heads/{branch}"),
        ],
    )
    .ok()
    .and_then(|log| log.lines().last().map(str::to_string))
    .filter(|sha| !sha.is_empty())
    .unwrap_or_else(|| fallback.to_string())
}

pub(super) fn resolve_existing_base(root: &Path, branch: &str) -> Result<(String, String)> {
    let base = git(
        root,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    )
    .or_else(|_| git(root, &["symbolic-ref", "--short", "HEAD"]))
    .unwrap_or_else(|_| branch.to_string());
    let tip = git(
        root,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
    )?;
    Ok((base, branch_base(root, branch, &tip)))
}

/// The branch a worktree dir is checked out on, None only when it is absent or detached.
pub(super) fn worktree_branch(dir: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(Error::rejected(format!(
                "{} is not a real worktree directory",
                dir.display()
            )))
        }
        Ok(_) => finish::git_branch(dir),
    }
}
