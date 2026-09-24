//! `cadence project new` (CAD-358): register a repo as a project and
//! seed its `PROJECT.md` — goal, the staffing `agents:` map
//! (docs/design/AGENT-FILESYSTEM.md), the default stages and empty
//! milestones (docs/design/WORK-MODEL.md) — in one tracker commit.
//!
//! The write runs in the daemon (`project_new` RPC), which decides who
//! the caller is; this module is the tracker side and trusts the actor
//! it is handed. `project.yaml` gets only the keys `project add` already
//! writes (`key`, `prefix`, `repos`) — it is `deny_unknown_fields`, and
//! an older binary would refuse a new key. The seeded gate keys equal
//! the defaults, so no `approve-work` step is needed.
//!
//! Idempotent: a second run with the same key and repo changes nothing.
//! Every refusal — a different repo for an existing key, a repo another
//! project owns, the reserved key `agents`, an invalid key or prefix, a
//! path that is not a git checkout — happens before anything is written.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{model, project, work, write, Pm};

/// Keys a project may never take: `<pm>/agents/` holds the agent files
/// (AGENT-FILESYSTEM.md; the master's `agents/master/`, CAD-339).
pub const RESERVED_KEYS: &[&str] = &["agents"];

/// Staffing seeded when the caller names none: one session each.
pub const DEFAULT_AGENTS: &[(&str, u32)] = &[("pm", 1), ("dev", 1), ("qa", 1)];

/// Most concurrent sessions one agent may be staffed with.
pub const MAX_SESSIONS: u32 = 16;

/// What `project new` was asked for.
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub key: String,
    /// Absolute path inside the repo's checkout (the CLI resolves it).
    pub repo: PathBuf,
    /// Issue id prefix; derived from the key when absent.
    pub prefix: Option<String>,
    /// The goal paragraph seeded into PROJECT.md.
    pub goal: Option<String>,
    /// `slug=n` staffing; [`DEFAULT_AGENTS`] when empty.
    pub agents: Vec<String>,
    /// The issue the project is created for — its `Issue:` trailer.
    pub issue: Option<String>,
}

/// A project key `project new` accepts: well formed and not reserved.
pub fn check_key(key: &str) -> Result<()> {
    model::check_key(key)?;
    if RESERVED_KEYS.contains(&key) {
        return Err(Error::rejected(format!(
            "Project key '{key}' is reserved — <pm>/{key}/ holds the agent files"
        )));
    }
    Ok(())
}

/// `CADENCE` → valid; an issue prefix is 1-8 uppercase letters/digits
/// starting with a letter.
pub fn check_prefix(prefix: &str) -> Result<()> {
    let ok = !prefix.is_empty()
        && prefix.len() <= 8
        && prefix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && prefix.starts_with(|c: char| c.is_ascii_uppercase());
    if ok {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "Invalid prefix '{prefix}' — uppercase letters/digits starting with a letter"
        )))
    }
}

/// The key's first three letters/digits, uppercased (`reminders` →
/// `REM`); a key that starts with a digit needs `--prefix`.
fn derive_prefix(key: &str) -> Result<String> {
    let prefix: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(3)
        .collect::<String>()
        .to_ascii_uppercase();
    check_prefix(&prefix).map_err(|_| {
        Error::rejected(format!(
            "Cannot derive an issue prefix from '{key}' — pass --prefix"
        ))
    })?;
    Ok(prefix)
}

/// `pm=1,dev=4` or repeated `slug=n` → an ordered, de-duplicated map.
fn parse_agents(specs: &[String]) -> Result<Vec<(String, u32)>> {
    if specs.is_empty() {
        return Ok(DEFAULT_AGENTS
            .iter()
            .map(|(s, n)| (s.to_string(), *n))
            .collect());
    }
    let mut out: Vec<(String, u32)> = Vec::new();
    for spec in specs.iter().flat_map(|s| s.split(',')) {
        let spec = spec.trim();
        let parsed = spec
            .split_once('=')
            .and_then(|(slug, n)| Some((slug.trim(), n.trim().parse::<u32>().ok()?)));
        let Some((slug, n)) = parsed else {
            return Err(Error::rejected(format!(
                "Invalid --agent '{spec}' — <agent>=<sessions>, e.g. dev=2"
            )));
        };
        if !model::valid_tag(slug) || slug == "operator" {
            return Err(Error::rejected(format!(
                "Invalid agent '{slug}' — 1-32 lowercase letters, digits or hyphens"
            )));
        }
        if n == 0 || n > MAX_SESSIONS {
            return Err(Error::rejected(format!(
                "Agent '{slug}' staffed with {n} sessions — 1-{MAX_SESSIONS}"
            )));
        }
        if out.iter().any(|(s, _)| s == slug) {
            return Err(Error::rejected(format!("Agent '{slug}' is staffed twice")));
        }
        out.push((slug.to_string(), n));
    }
    Ok(out)
}

/// The seeded `PROJECT.md`: the staffing map and the work keys in the
/// frontmatter (the work keys spelled from the defaults, so its gate
/// digest is the default one), the goal in the body.
pub fn seed(key: &str, goal: Option<&str>, agents: &[(String, u32)]) -> String {
    let agents = agents
        .iter()
        .map(|(s, n)| format!("{s}: {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let stages = work::DEFAULT_STAGES
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>()
        .join(", ");
    let goal = goal
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .unwrap_or("TODO — one paragraph: what this project is for and how we know it worked.");
    format!(
        "---\n\
         project: {key}\n\
         agents: {{{agents}}}      # max concurrent sessions per agent\n\
         stages: [{stages}]\n\
         stage_limit_days: {limit}\n\
         operator_stages: [{ops}]\n\
         milestones: []\n\
         ---\n\
         # {key}\n\
         \n\
         ## Goal\n\
         \n\
         {goal}\n\
         \n\
         ## Non-goals\n\
         \n\
         ## Context\n\
         \n\
         Architecture, ADRs and runbooks — links. Repos are registered in project.yaml.\n",
        limit = work::DEFAULT_STAGE_LIMIT_DAYS,
        ops = work::DEFAULT_OPERATOR_STAGES.join(", "),
    )
}

/// Does `repo` (a project.yaml entry) name the checkout at `root` or
/// the same origin remote.
fn same_repo(repo: &project::Repo, root: &Path, remote: Option<&str>) -> bool {
    let by_path = repo.path.as_deref().is_some_and(|p| {
        let path = project::expand_home(p);
        path.canonicalize().unwrap_or(path) == root
    });
    let by_remote = match (repo.remote.as_deref(), remote) {
        (Some(a), Some(b)) => project::normalize_remote(a) == b,
        _ => false,
    };
    by_path || by_remote
}

/// Register the repo and seed PROJECT.md in one tracker commit, as
/// `actor` (the daemon's verdict on the caller). Returns what was done;
/// `changed: false` when the project was already this repo's.
pub fn run(pm: &Pm, req: &Request, actor: &str) -> Result<Value> {
    let key = req.key.as_str();
    check_key(key)?;
    let prefix = match &req.prefix {
        Some(p) => {
            let p = p.to_ascii_uppercase();
            check_prefix(&p)?;
            Some(p)
        }
        None => None,
    };
    let agents = parse_agents(&req.agents)?;
    if let Some(goal) = &req.goal {
        crate::secret::guard_with(
            &format!("project {key}: goal"),
            goal,
            &crate::secret::Allowlist::default(),
        )?;
    }
    if !req.repo.is_absolute() {
        return Err(Error::rejected(format!(
            "Repo path '{}' must be absolute",
            req.repo.display()
        )));
    }
    let not_git = || {
        Error::rejected(format!(
            "{} is not a git repo with a checkout — `git init` it first",
            req.repo.display()
        ))
    };
    let canonical = req.repo.canonicalize().map_err(|_| not_git())?;
    let (root, remote) = project::repo_identity(&canonical).ok_or_else(not_git)?;
    // The tracker is never a project's repo: cwd resolution would read
    // the tracker as that project and its lanes would be worktrees of
    // the tracker. Compared canonically, so a symlink cannot hide it.
    let tracker = pm.dir.canonicalize().unwrap_or_else(|_| pm.dir.clone());
    if root.starts_with(&tracker) || tracker.starts_with(&root) || canonical.starts_with(&tracker) {
        return Err(Error::rejected(format!(
            "{} is the tracker ({}), inside it or contains it — a project's repo \
             must be its own checkout; nothing written",
            req.repo.display(),
            tracker.display()
        )));
    }

    let _lock = pm.lock()?;
    let projects = project::list(&pm.dir)?;
    if let Some(other) = projects.iter().find(|p| {
        p.key != key
            && p.repos
                .iter()
                .any(|r| same_repo(r, &root, remote.as_deref()))
    }) {
        return Err(Error::rejected(format!(
            "{} is already project '{}' — one repo, one project",
            root.display(),
            other.key
        )));
    }
    let existing = projects.iter().find(|p| p.key == key);
    if let Some(p) = existing {
        if !p
            .repos
            .iter()
            .any(|r| same_repo(r, &root, remote.as_deref()))
        {
            let known: Vec<&str> = p.repos.iter().filter_map(|r| r.path.as_deref()).collect();
            return Err(Error::rejected(format!(
                "Project '{key}' exists with a different repo ({}) — nothing written",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            )));
        }
        if prefix.as_deref().is_some_and(|want| want != p.prefix) {
            return Err(Error::rejected(format!(
                "Project '{key}' exists with prefix {} — nothing written",
                p.prefix
            )));
        }
    }
    let prefix = match (existing, prefix) {
        (Some(p), _) => p.prefix.clone(),
        (None, Some(p)) => p,
        (None, None) => derive_prefix(key)?,
    };
    if let Some(other) = projects.iter().find(|p| p.key != key && p.prefix == prefix) {
        return Err(Error::rejected(format!(
            "Prefix {prefix} is project '{}''s — pass another --prefix",
            other.key
        )));
    }
    if let Some(id) = &req.issue {
        write::issue_dir(pm, id)?;
    }

    let dir = pm.dir.join(key);
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{key}/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    let manifest = work::config_file(&pm.dir, key);
    // A PROJECT.md already there is the project's own — never replaced.
    let write_manifest = match manifest.symlink_metadata() {
        Ok(m) if m.is_file() => false,
        Ok(_) => {
            return Err(Error::rejected(format!(
                "{} is not a regular file — nothing written",
                manifest.display()
            )))
        }
        Err(_) => true,
    };
    let write_yaml = existing.is_none();
    if !write_yaml && !write_manifest {
        return Ok(json!({
            "project": key, "prefix": prefix, "path": dir,
            "repo": root, "changed": false, "committed": false,
        }));
    }

    // Everything checked — write, then commit; a failed commit removes
    // what this call created, so a refusal leaves nothing behind.
    let created_dir = !dir.exists();
    let mut created: Vec<PathBuf> = Vec::new();
    let result = (|| -> Result<()> {
        std::fs::create_dir_all(&dir)?;
        if write_yaml {
            let yaml = serde_yaml::to_string(&project::Project {
                key: key.to_string(),
                prefix: prefix.clone(),
                repos: vec![project::Repo {
                    path: Some(root.to_string_lossy().to_string()),
                    remote: remote.clone(),
                }],
                components: vec![],
                tags: vec![],
                default_owner: None,
                build: None,
                memory: None,
            })
            .map_err(|e| Error::internal(format!("project.yaml: {e}")))?;
            let file = dir.join("project.yaml");
            create_new(&file, &yaml)?;
            created.push(file);
        }
        if write_manifest {
            create_new(&manifest, &seed(key, req.goal.as_deref(), &agents))?;
            created.push(manifest.clone());
        }
        let what = if write_yaml {
            "registered"
        } else {
            "PROJECT.md seeded"
        };
        let ids: Vec<&str> = req.issue.iter().map(String::as_str).collect();
        write::commit(pm, &format!("project {key} {what}"), &ids, actor)
    })();
    if let Err(e) = result {
        // Unstage first (the commit's `git add -A` staged them), then
        // remove: a failed call leaves neither index entries nor files.
        let mut unstage: Vec<String> = vec!["reset".into(), "-q".into(), "--".into()];
        unstage.extend(
            created
                .iter()
                .filter_map(|f| f.strip_prefix(&pm.dir).ok())
                .map(|f| f.to_string_lossy().to_string()),
        );
        if unstage.len() > 3 {
            let args: Vec<&str> = unstage.iter().map(String::as_str).collect();
            let _ = crate::issue::git(&pm.dir, &args);
        }
        for file in created.iter().rev() {
            let _ = std::fs::remove_file(file);
        }
        if created_dir {
            let _ = std::fs::remove_dir(&dir);
        }
        // `e` carries git's stderr (a refusing hook's output included).
        return Err(Error::rejected(format!(
            "project {key}: the tracker commit failed, nothing kept — {e}"
        )));
    }
    Ok(json!({
        "project": key, "prefix": prefix, "path": dir, "repo": root,
        "remote": remote, "manifest": manifest,
        "agents": agents.iter().map(|(s, n)| (s.clone(), json!(n))).collect::<serde_json::Map<_, _>>(),
        "seeded": write_manifest, "changed": true, "committed": true, "actor": actor,
    }))
}

fn create_new(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn repo(dir: &Path, name: &str) -> PathBuf {
        let repo = dir.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        repo.canonicalize().unwrap()
    }

    fn pm(dir: &Path) -> Pm {
        Pm::init(&dir.join("pm")).unwrap()
    }

    fn commits(pm: &Pm) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&pm.dir)
            .args(["log", "--format=%H"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn req(key: &str, repo: &Path) -> Request {
        Request {
            key: key.into(),
            repo: repo.to_path_buf(),
            ..Request::default()
        }
    }

    #[test]
    fn seed_parses_to_default_gates_and_empty_milestones() {
        let text = seed("demo", Some("Ship it."), &parse_agents(&[]).unwrap());
        let cfg = work::parse_config(&text).unwrap();
        assert!(work::gates_default(&cfg), "{text}");
        assert_eq!(cfg, work::WorkConfig::default());
        let (yaml, body) = crate::issue::parse::split_front(&text).unwrap();
        let front: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(front["agents"]["dev"], 1);
        assert_eq!(front["milestones"].as_sequence().map(Vec::len), Some(0));
        assert!(body.contains("## Goal\n\nShip it."), "{body}");
    }

    #[test]
    fn agents_are_checked() {
        let got = parse_agents(&["pm=1,dev=4".into(), "qa=1".into()]).unwrap();
        assert_eq!(got, [("pm".into(), 1), ("dev".into(), 4), ("qa".into(), 1)]);
        for bad in [
            "dev",
            "dev=0",
            "dev=17",
            "Dev=1",
            "dev=1,dev=2",
            "operator=1",
        ] {
            assert!(parse_agents(&[bad.into()]).is_err(), "{bad}");
        }
    }

    #[test]
    fn prefix_is_derived_or_refused() {
        assert_eq!(derive_prefix("reminders").unwrap(), "REM");
        assert_eq!(derive_prefix("a-b").unwrap(), "AB");
        assert!(derive_prefix("3d").is_err());
    }

    #[test]
    fn registers_once_and_refuses_with_nothing_written() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pm = pm(tmp.path());
        let a = repo(tmp.path(), "a");
        let b = repo(tmp.path(), "b");
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let tracker_link = tmp.path().join("tracker-link");
        std::os::unix::fs::symlink(&pm.dir, &tracker_link).unwrap();
        let out = run(&pm, &req("demo", &a), "operator").unwrap();
        assert_eq!(out["changed"], true, "{out}");
        assert_eq!(out["prefix"], "DEM");
        let yaml = project::load(&pm.dir.join("demo/project.yaml")).unwrap();
        assert_eq!(yaml.repos[0].path.as_deref(), a.to_str());
        let after = commits(&pm);

        // Same key and repo (even through a subdirectory): no change.
        std::fs::create_dir_all(a.join("sub")).unwrap();
        for path in [a.clone(), a.join("sub")] {
            let out = run(&pm, &req("demo", &path), "operator").unwrap();
            assert_eq!(out["changed"], false, "{out}");
        }
        assert_eq!(commits(&pm), after);

        let refusals = [
            (req("demo", &b), "different repo"),
            (req("other", &a), "already project 'demo'"),
            (req("agents", &b), "reserved"),
            (req("Bad_Key", &b), "Invalid project key"),
            (req("fresh", &plain), "not a git repo"),
            (req("fresh", &tmp.path().join("missing")), "not a git repo"),
            (req("fresh", Path::new("rel")), "must be absolute"),
            (req("fresh", &pm.dir), "is the tracker"),
            (req("fresh", &pm.dir.join("demo")), "is the tracker"),
            (req("fresh", &tracker_link), "is the tracker"),
            (
                Request {
                    prefix: Some("DEM".into()),
                    ..req("fresh", &b)
                },
                "Prefix DEM",
            ),
            (
                Request {
                    prefix: Some("XYZ".into()),
                    ..req("demo", &a)
                },
                "prefix DEM",
            ),
            (
                Request {
                    issue: Some("DEM-9".into()),
                    ..req("fresh", &b)
                },
                "Unknown issue",
            ),
        ];
        for (r, want) in refusals {
            let err = run(&pm, &r, "operator").unwrap_err().to_string();
            assert!(err.contains(want), "{want}: {err}");
        }
        assert_eq!(commits(&pm), after, "a refusal commits nothing");
        for key in ["other", "agents", "fresh"] {
            assert!(!pm.dir.join(key).exists(), "{key}");
        }
    }

    #[test]
    fn seeds_the_manifest_of_a_project_added_without_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pm = pm(tmp.path());
        let a = repo(tmp.path(), "a");
        write::project_add(
            &pm,
            "demo",
            "D",
            &[a.to_string_lossy().to_string()],
            &[],
            &[],
            None,
        )
        .unwrap();
        let out = run(&pm, &req("demo", &a), "master").unwrap();
        assert_eq!(out["changed"], true, "{out}");
        assert_eq!(out["prefix"], "D");
        assert!(work::config_file(&pm.dir, "demo").is_file());
        let out = run(&pm, &req("demo", &a), "master").unwrap();
        assert_eq!(out["changed"], false, "{out}");
    }

    /// A repo that contains the tracker is refused too (the tracker
    /// nested in a product checkout).
    #[test]
    fn a_repo_containing_the_tracker_is_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let outer = repo(tmp.path(), "outer");
        let pm = Pm::init(&outer.join("pm")).unwrap();
        let err = run(&pm, &req("outer", &outer), "operator")
            .unwrap_err()
            .to_string();
        assert!(err.contains("is the tracker"), "{err}");
        assert!(!pm.dir.join("outer").exists());
    }

    /// A refused tracker commit (here a pre-commit hook) leaves nothing
    /// staged and nothing on disk, and the error carries git's stderr.
    #[test]
    fn a_failed_commit_unstages_and_removes_what_it_created() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let pm = pm(tmp.path());
        let a = repo(tmp.path(), "a");
        let hook = pm.dir.join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\necho 'lint says no' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let before = commits(&pm);
        let err = run(&pm, &req("demo", &a), "operator")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("lint says no") && err.contains("nothing kept"),
            "{err}"
        );
        assert_eq!(commits(&pm), before);
        assert!(!pm.dir.join("demo").exists());
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&pm.dir)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(status.stdout.is_empty(), "{status:?}");
    }

    /// Concurrent runs for one key serialize on the tracker lock:
    /// exactly one registers and commits, the rest see it done.
    #[test]
    fn concurrent_runs_commit_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pm_dir = pm(tmp.path()).dir;
        let a = repo(tmp.path(), "a");
        let before = commits(&Pm::at(&pm_dir).unwrap()).lines().count();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let (pm_dir, a, barrier) = (pm_dir.clone(), a.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let pm = Pm::at(&pm_dir).unwrap();
                    barrier.wait();
                    run(&pm, &req("demo", &a), "operator")
                })
            })
            .collect();
        let outs: Vec<Value> = handles
            .into_iter()
            .map(|h| h.join().unwrap().unwrap())
            .collect();
        let changed = outs.iter().filter(|o| o["changed"] == true).count();
        assert_eq!(changed, 1, "{outs:?}");
        let pm = Pm::at(&pm_dir).unwrap();
        assert_eq!(commits(&pm).lines().count(), before + 1);
    }

    #[test]
    fn project_add_refuses_the_reserved_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pm = pm(tmp.path());
        let err = write::project_add(&pm, "agents", "AG", &[], &[], &[], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("reserved"), "{err}");
        assert!(!pm.dir.join("agents").exists());
    }
}
