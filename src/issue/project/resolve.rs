//! Project discovery, loading, CWD/issue resolution, and observation.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use super::Project;
use crate::error::{Error, Result};

/// `~` → `$HOME`, else the path unchanged.
pub fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Normalise a git remote to `host/owner/repo` — `git@github.com:o/r.git`
/// and `https://github.com/o/r` agree.
pub fn normalize_remote(remote: &str) -> String {
    let mut r = remote.trim().to_string();
    for scheme in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = r.strip_prefix(scheme) {
            r = rest.to_string();
        }
    }
    // scp-style `git@host:path` → `host/path`
    if let Some(at) = r.find('@') {
        let (user, rest) = r.split_at(at);
        if !user.contains('/') {
            r = rest[1..].replacen(':', "/", 1);
        }
    }
    while r.ends_with('/') {
        r.pop();
    }
    if let Some(stripped) = r.strip_suffix(".git") {
        r = stripped.to_string();
    }
    r
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = crate::reaper::output(Command::new("git").arg("-C").arg(dir).args(args)).ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// The cwd's repo identity: `(main checkout root, normalised remote)`.
/// The main checkout root comes from `--git-common-dir` so every
/// worktree maps to the same identity.
#[doc(hidden)]
pub fn repo_identity(cwd: &Path) -> Option<(PathBuf, Option<String>)> {
    let common = git(cwd, &["rev-parse", "--git-common-dir"])?;
    let common = if Path::new(&common).is_absolute() {
        PathBuf::from(common)
    } else {
        cwd.join(common)
    };
    let common = common.canonicalize().unwrap_or(common);
    // <main>/.git for a normal repo or a linked worktree's common dir;
    // a bare common dir has no checkout root.
    let root = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent()?.to_path_buf()
    } else {
        git(cwd, &["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)?
            .canonicalize()
            .ok()?
    };
    let remote = git(cwd, &["config", "--get", "remote.origin.url"])
        .filter(|r| !r.is_empty())
        .map(|r| normalize_remote(&r));
    Some((root, remote))
}

/// Load every `<pm>/<key>/project.yaml`, sorted by key.
///
/// An unreadable tracker answers an empty list, and an entry whose type
/// cannot be read is skipped. That is right for callers that only render what
/// they could read, and **wrong for access decisions**: an empty list reads as
/// "nothing is restricted". Authorization uses [`list_checked`].
pub fn list(pm_dir: &Path) -> Result<Vec<Project>> {
    let mut projects = Vec::new();
    let Ok(entries) = std::fs::read_dir(pm_dir) else {
        return Ok(projects);
    };
    for entry in entries.flatten() {
        // lstat-style: a symlinked project dir or project.yaml is not
        // a project — links could point outside the PM dir.
        let dir_ok = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let file = entry.path().join("project.yaml");
        if dir_ok && crate::issue::board::is_real_file(&file) {
            projects.push(load(&file)?);
        }
    }
    projects.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(projects)
}

/// [`list`] for an access decision (CAD-1950).
///
/// Identical rows, but an answer that cannot be trusted is an error rather
/// than an empty list: an unreadable tracker directory, or an entry whose type
/// cannot be read, might be hiding a restricted project. A tracker that does
/// not exist is `Ok(empty)` — nothing is there to hide — so a fresh checkout
/// behaves as before. Callers that must fail closed map the error to "no
/// projects", which refuses every project rather than admitting all of them.
pub fn list_checked(pm_dir: &Path) -> Result<Vec<Project>> {
    let entries = match std::fs::read_dir(pm_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(Error::internal(format!(
                "cannot read the tracker {}: {e}",
                pm_dir.display()
            )))
        }
    };
    let mut projects = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| {
            Error::internal(format!(
                "cannot read an entry of the tracker {}: {e}",
                pm_dir.display()
            ))
        })?;
        // lstat-style, as `list`: a symlinked project dir or project.yaml is
        // not a project.
        let kind = entry.file_type().map_err(|e| {
            Error::internal(format!(
                "cannot classify {} in the tracker: {e}",
                entry.path().display()
            ))
        })?;
        let file = entry.path().join("project.yaml");
        if !kind.is_dir() {
            continue;
        }
        // `is_real_file` answers false for *any* metadata error, so a project
        // directory that cannot be searched is silently omitted — the same
        // fail-open this listing exists to close, one level down. Distinguish
        // "no project.yaml here" (not a project) from "cannot tell".
        let meta = match std::fs::symlink_metadata(&file) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(Error::internal(format!(
                    "cannot inspect {}: {e}",
                    file.display()
                )))
            }
        };
        // lstat-style, as `list`: a symlinked or non-regular project.yaml is
        // not a project.
        if meta.is_file() && !meta.file_type().is_symlink() {
            let project = load(&file)?;
            let name = entry.file_name();
            if !crate::issue::model::valid_key(&project.key)
                || name.to_str() != Some(project.key.as_str())
            {
                return Err(Error::internal(format!(
                    "project key does not match its tracker directory: {}",
                    file.display()
                )));
            }
            projects.push(project);
        }
    }
    projects.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(projects)
}

pub fn load(file: &Path) -> Result<Project> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| Error::internal(format!("cannot read {}: {e}", file.display())))?;
    let project: Project = serde_yaml::from_str(&text).map_err(|e| {
        Error::internal(format!("{} is not valid project.yaml: {e}", file.display()))
    })?;
    if project.id.as_deref() == Some("") {
        return Err(Error::rejected(format!(
            "{} records an empty project id; remove it or mint a new one",
            file.display()
        )));
    }
    Ok(project)
}

/// Every project whose `repos` claim this checkout — a `repos[].remote`
/// equal to the checkout's origin, then a `repos[].path` equal to its
/// root — sorted by key. A repo registered by several projects lists
/// every claimant: resolution must never pick the first one (CAD-2001).
fn claimants<'a>(projects: &'a [Project], root: &Path, remote: Option<&str>) -> Vec<&'a Project> {
    let mut hits: Vec<&Project> = Vec::new();
    if let Some(remote) = remote {
        hits.extend(projects.iter().filter(|p| {
            p.repos.iter().any(|r| {
                r.remote
                    .as_deref()
                    .is_some_and(|rr| normalize_remote(rr) == remote)
            })
        }));
    }
    for project in projects {
        if hits.iter().any(|hit| hit.key == project.key) {
            continue;
        }
        let claims = project.repos.iter().any(|r| {
            r.path.as_ref().is_some_and(|path| {
                let path = expand_home(path);
                path.canonicalize().unwrap_or(path) == root
            })
        });
        if claims {
            hits.push(project);
        }
    }
    hits.sort_by(|a, b| a.key.cmp(&b.key));
    hits
}

/// The project keys claiming a cwd's checkout, in key order. Empty when
/// the cwd is not a git checkout or no project claims it. Read-only and
/// best-effort: an unreadable tracker claims nothing.
pub fn claimants_for_cwd(pm_dir: &Path, cwd: &Path) -> Vec<String> {
    let Ok(projects) = list(pm_dir) else {
        return Vec::new();
    };
    claimants_for(&projects, cwd)
}

/// [`claimants_for_cwd`] against an already-loaded project list — a
/// caller that read the tracker once does not re-read it per agent.
pub fn claimants_for(projects: &[Project], cwd: &Path) -> Vec<String> {
    let Some((root, remote)) = repo_identity(cwd) else {
        return Vec::new();
    };
    claimants(projects, &root, remote.as_deref())
        .into_iter()
        .map(|p| p.key.clone())
        .collect()
}

/// The project key a cwd's repo identity resolves to — remote first,
/// then checkout path — or `None` when nothing claims it **or more than
/// one project does**. A repo registered by several projects is
/// ambiguous, and this signature cannot report that: a caller that must
/// explain the refusal uses [`resolve`] or [`claimants_for_cwd`].
/// Read-only; `agent list --project` maps each row's `cwd` through this.
pub fn key_for_cwd(pm_dir: &Path, cwd: &Path) -> Option<String> {
    match claimants_for_cwd(pm_dir, cwd).as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// The project key owning an issue id, by its `<PREFIX>-<n>` prefix.
/// `None` for a malformed id, an unknown prefix, or a prefix two
/// projects share — the id then names no single project.
pub fn key_for_issue(pm_dir: &Path, issue_id: &str) -> Option<String> {
    let (prefix, _) = issue_id.rsplit_once('-')?;
    let mut keys = list(pm_dir)
        .ok()?
        .into_iter()
        .filter(|p| p.prefix == prefix)
        .map(|p| p.key);
    let first = keys.next()?;
    keys.next().is_none().then_some(first)
}

/// Resolve the project an issue command files into: `--project` flag,
/// then `CADENCE_PROJECT`, then the cwd's git identity against every
/// project.yaml (remote first, then checkout path). A named project is
/// authoritative even when its repo is shared. A cwd claimed by several
/// projects is refused by name — never first-match. No match fails
/// closed with the known project list.
pub fn resolve(pm_dir: &Path, flag: Option<&str>, cwd: &Path) -> Result<Project> {
    let env = std::env::var("CADENCE_PROJECT").ok();
    resolve_with(pm_dir, flag, env.as_deref(), cwd)
}

/// [`resolve`] with the ambient `CADENCE_PROJECT` passed in, so the
/// precedence order is one testable place: flag, then `named_env`, then
/// the cwd.
pub(super) fn resolve_with(
    pm_dir: &Path,
    flag: Option<&str>,
    named_env: Option<&str>,
    cwd: &Path,
) -> Result<Project> {
    let projects = list(pm_dir)?;
    let named = flag
        .map(str::to_string)
        .or_else(|| named_env.map(str::to_string))
        .filter(|s| !s.is_empty());
    if let Some(name) = named {
        return projects
            .into_iter()
            .find(|p| p.key == name)
            .ok_or_else(|| unknown_project(&name, pm_dir));
    }
    let Some((root, remote)) = repo_identity(cwd) else {
        return Err(unknown_project("(no git repo at cwd)", pm_dir));
    };
    match claimants(&projects, &root, remote.as_deref()).as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(unknown_project(
            &format!("(cwd repo {} matched nothing)", root.display()),
            pm_dir,
        )),
        many => Err(ambiguous_project(
            &root,
            &many.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(),
        )),
    }
}

/// A cwd its projects cannot answer for: refuse by name rather than
/// pick a first claimant, and point at the explicit selectors.
fn ambiguous_project(root: &Path, keys: &[&str]) -> Error {
    Error::rejected(format!(
        "{} is registered by {} projects ({}) — pass --project <key> or set \
         CADENCE_PROJECT; a repo shared by several projects has no single project",
        root.display(),
        keys.len(),
        keys.join(", ")
    ))
}

/// Read-only project scope for setup. Never creates a git repo, a
/// tracker, or a project registration. `named` is `CADENCE_PROJECT`
/// when the caller has one. A cwd claimed by several projects is
/// reported as `ambiguous: true` with its `candidates`, never resolved
/// to the first claimant.
pub fn observe(pm_dir: &Path, cwd: &Path, named: Option<&str>) -> Value {
    let register = "cadence issue project add <key> --prefix <P> --repo <path>";
    if let Some(name) = named.map(str::trim).filter(|name| !name.is_empty()) {
        let projects = list(pm_dir).unwrap_or_default();
        if let Some(project) = projects.iter().find(|project| project.key == name) {
            return json!({
                "resolved": true,
                "source": "CADENCE_PROJECT",
                "key": project.key,
                "register": Value::Null,
            });
        }
        return json!({
            "resolved": false,
            "source": "CADENCE_PROJECT",
            "key": name,
            "detail": format!("CADENCE_PROJECT={name} is not a registered project"),
            "register": register,
        });
    }
    let Some((root, remote)) = repo_identity(cwd) else {
        return json!({
            "resolved": false,
            "source": "cwd",
            "detail": "cwd is not a git checkout; it was not initialized or registered",
            "register": register,
        });
    };
    let projects = list(pm_dir).unwrap_or_default();
    match claimants(&projects, &root, remote.as_deref()).as_slice() {
        [one] => json!({
            "resolved": true,
            "source": "cwd",
            "key": one.key,
            "repo": root,
            "remote": remote,
            "register": Value::Null,
        }),
        [] => json!({
            "resolved": false,
            "source": "cwd",
            "repo": root,
            "remote": remote,
            "detail": "cwd git identity matched no registered project",
            "register": register,
        }),
        // CAD-2001: a shared repo names no single project. Report the
        // claimants instead of resolving to the first one, and offer the
        // explicit selector rather than a registration this checkout
        // does not need.
        many => json!({
            "resolved": false,
            "source": "cwd",
            "ambiguous": true,
            "candidates": many.iter().map(|p| p.key.clone()).collect::<Vec<_>>(),
            "repo": root,
            "remote": remote,
            "detail": format!(
                "cwd git identity is registered by {} projects ({}) — set \
                 CADENCE_PROJECT or pass --project; refusing to pick one",
                many.len(),
                many.iter()
                    .map(|p| p.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "register": Value::Null,
        }),
    }
}

#[doc(hidden)]
pub fn unknown_project(name: &str, pm_dir: &Path) -> Error {
    let known = list(pm_dir)
        .map(|ps| ps.iter().map(|p| p.key.clone()).collect::<Vec<_>>())
        .unwrap_or_default();
    Error::rejected(format!(
        "No project for {name} — known projects: {}. \
         Register one with `cadence issue project add <key> --prefix <P> --repo <path>` \
         or pass --project",
        if known.is_empty() {
            "none".to_string()
        } else {
            known.join(", ")
        }
    ))
}
