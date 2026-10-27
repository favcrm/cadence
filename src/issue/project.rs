//! `<pm>/<key>/project.yaml` and project resolution for the CLI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
    /// Integration branch `issue start` cuts a new lane from, when it
    /// is not the remote's default HEAD. `staging` and `origin/staging`
    /// both fetch `refs/heads/staging` and pin that commit. Absent:
    /// `origin/HEAD` when it is a symbolic ref, else the remote's
    /// symbolic HEAD. Never the main checkout's current branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
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
    /// CAD-139: idea pipeline. Absent means research stays off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intake: Option<IntakePolicy>,
    /// Immutable project identity (AOS-274). Minted once at creation;
    /// absent on projects that predate the policy contract, where it is
    /// backfilled without changing access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Recorded access policy. Absent means no policy was ever recorded,
    /// which preserves the project's current access (REQ-007).
    ///
    /// The policy lives in this file on purpose: an older Cadence refuses
    /// to parse a project it cannot authorize, instead of silently
    /// ignoring a policy that hides content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ProjectPolicy>,
}

/// `[intake]` on a project — the idea pipeline's budget switch.
/// `auto_research` defaults off so filing an idea spends nothing.
/// `max_per_day` defaults to 3 research starts per UTC day.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakePolicy {
    #[serde(default)]
    pub auto_research: bool,
    #[serde(default = "default_idea_max_per_day")]
    pub max_per_day: u32,
}

fn default_idea_max_per_day() -> u32 {
    3
}

impl Default for IntakePolicy {
    fn default() -> Self {
        Self {
            auto_research: false,
            max_per_day: default_idea_max_per_day(),
        }
    }
}

impl Project {
    pub fn intake_policy(&self) -> IntakePolicy {
        self.intake.clone().unwrap_or_default()
    }
}

/// Mint a new immutable project identity (AOS-274).
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Project visibility (AOS-274 / TASK-003). Exactly two values: there is
/// no third visibility, and project membership is not a role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    #[default]
    WorkspaceVisible,
    Private,
}

/// One grant on a private project, keyed to a caller spelling the daemon
/// already uses (`users/<sub>`, `operator`).
///
/// The reserved namespaces (`agents/…`, `service/…`) are refused at parse
/// time: a hand-edited policy must fail loudly rather than quietly widen
/// access to an agent principal (AOS-274 / SEC-006).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Grant(String);

impl Grant {
    pub fn parse(raw: &str) -> Result<Self> {
        // The bound is the derivations' cut ([`crate::ident::HANDLE_MAX`]):
        // a longer handle can never be produced from a subject, so a grant
        // carrying one could never match a caller — refused here rather
        // than recorded as a policy that silently never applies.
        let handle_ok = |handle: &str| crate::ident::is_handle(handle, crate::ident::HANDLE_MAX);
        let ok = raw == "operator" || raw.strip_prefix("users/").is_some_and(handle_ok);
        if !ok {
            return Err(Error::rejected(format!(
                "project policy grant {raw:?} is not a supported subject — \
                 use `operator` or `users/<handle>`"
            )));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Grant {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// The project's recorded access policy.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPolicy {
    /// Monotonic; advances on every effective change so a stale
    /// projection can refuse rather than serve the previous policy.
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub visibility: Visibility,
    /// Grants for a private project; ignored while workspace-visible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Grant>,
}

impl ProjectPolicy {
    /// The policy a newly created workspace-visible project gets.
    pub fn workspace_visible() -> Self {
        Self {
            version: 1,
            visibility: Visibility::WorkspaceVisible,
            members: Vec::new(),
        }
    }

    /// Does this policy admit `caller`? Owners keep administrative access,
    /// so an operator is always admitted; every other caller needs an
    /// explicit grant.
    pub fn allows(&self, caller: ProjectCaller<'_>) -> bool {
        match caller {
            ProjectCaller::Operator => true,
            ProjectCaller::User(handle) => self
                .members
                .iter()
                .any(|grant| grant.as_str().strip_prefix("users/") == Some(handle)),
            // Reserved namespaces are never admitted by a project grant.
            ProjectCaller::Agent(_) | ProjectCaller::Service(_) => false,
        }
    }
}

/// The caller a project decision is about, in the daemon's own spelling.
/// Each enforcement point maps its session or envelope to one of these;
/// the reserved variants exist so an agent or service caller is refused
/// deliberately instead of by accident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectCaller<'a> {
    /// `users/<sub>` — a human member of the workspace.
    User(&'a str),
    /// `operator` — administrative access, including the Owner.
    Operator,
    /// `agents/<id>` — reserved; refused on a private project.
    Agent(&'a str),
    /// `service/<sub>` — reserved; refused on a private project.
    Service(&'a str),
}

impl Project {
    /// May `caller` read or contribute to this project?
    ///
    /// A project with no recorded policy keeps today's access — it is not
    /// implicitly private (REQ-007). Everything a policy does not
    /// explicitly admit is refused.
    pub fn allows(&self, caller: ProjectCaller<'_>) -> bool {
        match &self.policy {
            None => true,
            Some(policy) if policy.visibility == Visibility::WorkspaceVisible => true,
            Some(policy) => policy.allows(caller),
        }
    }
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
    if project.id.as_deref() == Some("") {
        return Err(Error::rejected(format!(
            "{} records an empty project id; remove it or mint a new one",
            file.display()
        )));
    }
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

/// The project key owning an issue id, by its `<PREFIX>-<n>` prefix.
/// `None` for a malformed id or an unknown prefix.
pub fn key_for_issue(pm_dir: &Path, issue_id: &str) -> Option<String> {
    let (prefix, _) = issue_id.rsplit_once('-')?;
    list(pm_dir)
        .ok()?
        .into_iter()
        .find(|p| p.prefix == prefix)
        .map(|p| p.key)
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

/// Read-only project scope for setup. Never creates a git repo, a
/// tracker, or a project registration. `named` is `CADENCE_PROJECT`
/// when the caller has one.
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
    if let Some(key) = key_for_cwd(pm_dir, cwd) {
        return json!({
            "resolved": true,
            "source": "cwd",
            "key": key,
            "repo": root,
            "remote": remote,
            "register": Value::Null,
        });
    }
    json!({
        "resolved": false,
        "source": "cwd",
        "repo": root,
        "remote": remote,
        "detail": "cwd git identity matched no registered project",
        "register": register,
    })
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
    fn observe_does_not_initialize_a_non_git_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "keep\n").unwrap();
        let scope = observe(dir.path(), dir.path(), None);
        assert_eq!(scope["resolved"], false);
        assert!(scope["detail"]
            .as_str()
            .unwrap()
            .contains("not a git checkout"));
        assert!(!dir.path().join(".git").exists());
        assert_eq!(
            std::fs::read(dir.path().join("notes.txt")).unwrap(),
            b"keep\n"
        );
        assert!(scope["register"].as_str().unwrap().contains("project add"));
    }

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

    fn write_project(dir: &tempfile::TempDir, body: &str) -> PathBuf {
        let file = dir.path().join("project.yaml");
        std::fs::write(&file, format!("key: x\nprefix: X\n{body}")).unwrap();
        file
    }

    #[test]
    fn a_project_without_policy_keeps_todays_access() {
        let dir = tempfile::TempDir::new().unwrap();
        let project = load(&write_project(&dir, "")).unwrap();
        assert_eq!(project.id, None);
        assert_eq!(project.policy, None);
        for caller in [
            ProjectCaller::Operator,
            ProjectCaller::User("alice"),
            ProjectCaller::Agent("builder"),
            ProjectCaller::Service("svc"),
        ] {
            assert!(
                project.allows(caller),
                "{caller:?} refused without a policy"
            );
        }
    }

    #[test]
    fn a_private_project_admits_the_owner_and_granted_members_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = write_project(
            &dir,
            "policy:\n  version: 3\n  visibility: private\n  members:\n  - users/alice\n  - operator\n",
        );
        let project = load(&file).unwrap();
        let policy = project.policy.clone().unwrap();
        assert_eq!(policy.version, 3);
        assert_eq!(policy.visibility, Visibility::Private);
        // The Owner keeps administrative access; the stored `operator`
        // grant is redundant but harmless.
        assert!(project.allows(ProjectCaller::Operator));
        assert!(project.allows(ProjectCaller::User("alice")));
        assert!(!project.allows(ProjectCaller::User("mallory")));
        // Reserved namespaces are refused even when a caller spells them
        // like a granted member.
        assert!(!project.allows(ProjectCaller::Agent("alice")));
        assert!(!project.allows(ProjectCaller::Service("alice")));
    }

    #[test]
    fn a_workspace_visible_policy_never_narrows_access() {
        let dir = tempfile::TempDir::new().unwrap();
        let project = load(&write_project(&dir, "policy:\n  version: 1\n")).unwrap();
        assert_eq!(
            project.policy.clone().unwrap().visibility,
            Visibility::WorkspaceVisible
        );
        // An empty grant list is not an implicit deny while visible.
        assert!(project.allows(ProjectCaller::User("anyone")));
    }

    #[test]
    fn a_policy_refuses_unsupported_subjects_loudly() {
        let dir = tempfile::TempDir::new().unwrap();
        for member in [
            "agents/builder",
            "service/svc",
            "alice",
            "users/",
            "users/a b",
        ] {
            let file = write_project(
                &dir,
                &format!("policy:\n  visibility: private\n  members:\n  - {member}\n"),
            );
            let err = load(&file).unwrap_err().to_string();
            assert!(err.contains(member), "{member}: {err}");
        }
    }

    #[test]
    fn a_policy_refuses_unknown_keys_and_values() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = write_project(
            &dir,
            "policy:\n  version: 1\n  visibility: private\n  member:\n  - users/a\n",
        );
        let err = load(&file).unwrap_err().to_string();
        assert!(err.contains("member"), "{err}");

        let file = write_project(&dir, "policy:\n  visibility: secret\n");
        assert!(load(&file).is_err(), "an unknown visibility must refuse");

        let file = write_project(&dir, "id: \"\"\n");
        assert!(load(&file).is_err(), "an empty id must refuse");
    }

    #[test]
    fn identity_and_policy_round_trip() {
        let project = Project {
            key: "x".into(),
            prefix: "X".into(),
            repos: Vec::new(),
            components: Vec::new(),
            tags: Vec::new(),
            default_owner: None,
            build: None,
            memory: None,
            intake: None,
            id: Some(new_id()),
            policy: Some(ProjectPolicy::workspace_visible()),
        };
        let yaml = serde_yaml::to_string(&project).unwrap();
        let parsed: Project = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.id, project.id);
        assert_eq!(parsed.policy, project.policy);
        assert!(parsed.id.as_deref().is_some_and(|id| id.len() == 36));
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
