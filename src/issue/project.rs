//! `<pm>/<key>/project.yaml` and project resolution for the CLI.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub key: String,
    pub prefix: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<Repo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_owner: Option<String>,
}

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
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// The cwd's repo identity: `(main checkout root, normalised remote)`.
/// The main checkout root comes from `--git-common-dir` so every
/// worktree maps to the same identity.
pub(crate) fn repo_identity(cwd: &Path) -> Option<(PathBuf, Option<String>)> {
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

pub fn load(file: &Path) -> Result<Project> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| Error::internal(format!("cannot read {}: {e}", file.display())))?;
    let project: Project = serde_yaml::from_str(&text).map_err(|e| {
        Error::internal(format!("{} is not valid project.yaml: {e}", file.display()))
    })?;
    Ok(project)
}

/// Resolve the project an issue command files into: `--project` flag,
/// then `CADENCE_PROJECT`, then the cwd's git identity against every
/// project.yaml (remote first, then checkout path). No match fails
/// closed with the known project list.
pub fn resolve(pm_dir: &Path, flag: Option<&str>, cwd: &Path) -> Result<Project> {
    let projects = list(pm_dir)?;
    let named = flag
        .map(str::to_string)
        .or_else(|| std::env::var("CADENCE_PROJECT").ok())
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
    if let Some(remote) = &remote {
        for project in &projects {
            if project.repos.iter().any(|r| {
                r.remote
                    .as_deref()
                    .map(|p| normalize_remote(p) == *remote)
                    .unwrap_or(false)
            }) {
                return Ok(project.clone());
            }
        }
    }
    for project in &projects {
        for repo in &project.repos {
            if let Some(path) = &repo.path {
                let path = expand_home(path)
                    .canonicalize()
                    .unwrap_or_else(|_| expand_home(path));
                if path == root {
                    return Ok(project.clone());
                }
            }
        }
    }
    Err(unknown_project(
        &format!("(cwd repo {} matched nothing)", root.display()),
        pm_dir,
    ))
}

fn unknown_project(name: &str, pm_dir: &Path) -> Error {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_normalisation() {
        assert_eq!(
            normalize_remote("git@github.com:favcrm/cadence.git"),
            "github.com/favcrm/cadence"
        );
        assert_eq!(
            normalize_remote("https://github.com/favcrm/cadence.git"),
            "github.com/favcrm/cadence"
        );
        assert_eq!(
            normalize_remote("https://github.com/favcrm/cadence/"),
            "github.com/favcrm/cadence"
        );
        assert_eq!(
            normalize_remote("ssh://git@github.com/favcrm/cadence"),
            "github.com/favcrm/cadence"
        );
    }
}
