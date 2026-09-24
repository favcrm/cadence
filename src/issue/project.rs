//! `<pm>/<key>/project.yaml` and project resolution for the CLI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

// Unknown keys are refused on every project.yaml table: a misspelled
// policy key must fail loudly, not be dropped (CAD-170).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

/// `[build]` — per-project build knobs. `target_dir` picks how issue
/// worktrees store cargo output: `"shared"` (default) links each
/// lane's hashed `target/debug` subdirs into
/// `<repo>/.cadence/target/shared`, `"per-worktree"` keeps the
/// classic fully-private `target/` (the opt-out for hosts where the
/// cargo lock queue costs more than the disk it saves).
///
/// `recipes` (CAD-230b) are the only commands the daemon will launch
/// for this project (`cadence build-slot launch <name>`): a fixed argv,
/// a repo-relative cwd and an environment allowlist — never anything a
/// caller supplies.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_dir: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub recipes: BTreeMap<String, Recipe>,
}

/// One `build.recipes.<name>` entry — see [`crate::runner`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    /// The exact command; `argv[0]` is resolved against the allowlisted
    /// `PATH` (or the shell's default when `PATH` is not allowlisted).
    pub argv: Vec<String>,
    /// Repo-relative working directory (default: the checkout root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Environment NAMES passed through from the daemon's environment;
    /// nothing else reaches the command (`CADENCE_*` is refused).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// The slot pool: `build` (default), `test` or `suite`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub key: String,
    pub prefix: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<Repo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
    /// Declared tag vocabulary — empty accepts any well-formed tag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<Build>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryPolicy>,
}

/// `memory:` — project-memory retrieval policy (CAD-203).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryPolicy {
    /// Days a verified lesson's evidence stays fresh. A lesson last
    /// verified longer ago is not withheld: it decays to unverified and
    /// is still injected, labelled "unverified (last verified <date>)",
    /// until a new verify cycle is finalized. Only an explicit `stale:`
    /// mark withholds. Default: `memory::DEFAULT_STALE_DAYS` (30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_days: Option<u64>,
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

/// The project key a cwd's repo identity resolves to — remote first,
/// then checkout path, same as [`resolve`]'s cwd arm — or `None` when
/// nothing matches. Read-only; `agent list --project` maps each row's
/// `cwd` through this.
pub fn key_for_cwd(pm_dir: &Path, cwd: &Path) -> Option<String> {
    let (root, remote) = repo_identity(cwd)?;
    let projects = list(pm_dir).ok()?;
    if let Some(remote) = &remote {
        if let Some(p) = projects.iter().find(|p| {
            p.repos.iter().any(|r| {
                r.remote
                    .as_deref()
                    .map(|rr| normalize_remote(rr) == *remote)
                    .unwrap_or(false)
            })
        }) {
            return Some(p.key.clone());
        }
    }
    projects
        .iter()
        .find(|p| {
            p.repos.iter().any(|r| {
                r.path.as_ref().is_some_and(|path| {
                    let path = expand_home(path);
                    path.canonicalize().unwrap_or(path) == root
                })
            })
        })
        .map(|p| p.key.clone())
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

pub(crate) fn unknown_project(name: &str, pm_dir: &Path) -> Error {
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
    fn project_yaml_refuses_unknown_keys() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("project.yaml");
        std::fs::write(&file, "key: x\nprefix: X\nbuild:\n  target_dir: shared\n").unwrap();
        assert!(load(&file).is_ok());
        // A misspelled top-level or nested policy key names itself.
        for (yaml, key) in [
            ("key: x\nprefix: X\nworktrees:\n  root: /tmp\n", "worktrees"),
            (
                "key: x\nprefix: X\nbuild:\n  target-dir: shared\n",
                "target-dir",
            ),
            (
                "key: x\nprefix: X\nrepos:\n- path: /r\n  remot: r\n",
                "remot",
            ),
        ] {
            std::fs::write(&file, yaml).unwrap();
            let err = load(&file).unwrap_err().to_string();
            assert!(err.contains(key), "{key}: {err}");
        }
    }

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
