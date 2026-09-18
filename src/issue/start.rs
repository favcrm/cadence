//! `cadence issue start <ID>` — bind a project-repo worktree and
//! branch to an issue: mint `.cadence/wt/<id>-<slug>` on
//! `cadence/<id>-<slug>`, record both as refs, move `backlog|ready`
//! to `doing`, print the CAD-42 trailer. `--job` additionally opens
//! an M3 job whose task is already scoped to the worktree.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};
use crate::issue::model::Ref;
use crate::issue::{git, project, write, Pm};
use crate::{proto, worktree};

/// Optional M3 job creation: `--job --pm <alias> --spec <file>
/// [--assignee <alias>]`.
pub struct JobArgs {
    pub pm: String,
    pub spec: PathBuf,
    pub assignee: Option<String>,
}

pub struct StartArgs {
    pub repo: Option<PathBuf>,
    pub name: Option<String>,
    pub base: Option<String>,
    pub owner: Option<String>,
    pub job: Option<JobArgs>,
}

/// ASCII-lower `-`-separated slug, ≤32 chars — `New Login Form` →
/// `new-login-form`. Non-ASCII titles fall back to `work`.
fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !slug.is_empty() {
                slug.push('-');
            }
            dash = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            dash = true;
        }
    }
    let slug = slug.chars().take(32).collect::<String>();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "work".to_string()
    } else {
        slug.to_string()
    }
}

/// The project's declared repo roots, canonicalized.
fn declared_repos(project: &project::Project) -> Vec<PathBuf> {
    project
        .repos
        .iter()
        .filter_map(|r| r.path.as_deref())
        .map(|p| {
            project::expand_home(p)
                .canonicalize()
                .unwrap_or_else(|_| project::expand_home(p))
        })
        .collect()
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
fn resolve_repo(project: &project::Project, flag: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
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
    Err(Error::rejected(format!(
        "Which repo does this issue work in? Pass --repo — {}",
        repo_list(project, &candidates)
    )))
}

/// Base resolution per decision 1: `--base`, then the repo's
/// `origin/HEAD` target, then the current branch, then `HEAD`.
/// Returns `(ref, sha)`; no fetch.
fn resolve_base(root: &Path, flag: Option<&str>) -> Result<(String, String)> {
    let base = if let Some(b) = flag {
        b.to_string()
    } else if let Ok(origin_head) = git(
        root,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    ) {
        origin_head
    } else {
        git(root, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_else(|_| "HEAD".to_string())
    };
    let sha = git(
        root,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )
    .map_err(|_| {
        Error::rejected(format!(
            "Base '{base}' does not resolve in {} — pass --base <ref>",
            root.display()
        ))
    })?;
    Ok((base, sha))
}

/// The branch a worktree dir is checked out on (`symbolic-ref --short
/// HEAD`), or None when the dir is not a worktree.
fn worktree_branch(dir: &Path) -> Option<String> {
    git(dir, &["symbolic-ref", "--short", "HEAD"]).ok()
}

/// Probe the daemon, the PM alias and the assignee before anything is
/// created — `--job` refuses up front on any failure. The assignee
/// rule mirrors `check_group_member` (the PM itself or an agent whose
/// `params.upstream` names it): a bad assignee must never become a
/// post-commit failure.
fn check_job(state_dir: &Path, job: &JobArgs) -> Result<(PathBuf, String, String)> {
    client::rpc(state_dir, "agent_show", json!({"alias": job.pm}))?;
    if let Some(assignee) = &job.assignee {
        let agent = client::rpc(state_dir, "agent_show", json!({"alias": assignee}))
            .map_err(|e| Error::rejected(format!("Assignee '{assignee}': {e}")))?;
        let member = *assignee == job.pm
            || agent["agent"]["params"]["upstream"].as_str() == Some(job.pm.as_str());
        if !member {
            return Err(Error::rejected(format!(
                "'{assignee}' is not in '{}'s group — the assignee must \
                 be the PM itself or a member of its group",
                job.pm
            )));
        }
    }
    let spec = job
        .spec
        .canonicalize()
        .map_err(|_| Error::rejected(format!("Spec file {} is unreadable", job.spec.display())))?;
    let bytes = std::fs::read(&spec)?;
    use sha2::{Digest, Sha256};
    Ok((
        state_dir.to_path_buf(),
        spec.to_string_lossy().into_owned(),
        format!("{:x}", Sha256::digest(&bytes)),
    ))
}

pub fn run(pm: &Pm, id: &str, args: &StartArgs, actor: &str, state_dir: &Path) -> Result<Value> {
    // Daemon reachability + spec readability are checked before any
    // filesystem or tracker mutation — a `--job` that cannot be opened
    // must leave nothing behind.
    let job_probe = match &args.job {
        Some(job) => Some(check_job(state_dir, job)?),
        None => None,
    };
    let (project, dir) = write::issue_dir(pm, id)?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = resolve_repo(&project, args.repo.as_deref(), &cwd)?;
    let (base, base_sha) = resolve_base(&root, args.base.as_deref())?;

    let _lock = pm.lock()?;
    let (mut front, body) = write::load_front(&dir)?;
    let slug = match &args.name {
        Some(name) => proto::identifier(name, "--name")?,
        None => slugify(&front.title),
    };
    let wt_name = format!("{}-{}", front.id.to_lowercase(), slug);
    proto::identifier(&wt_name, "Worktree name")?;
    let branch = format!("cadence/{wt_name}");
    let wt_dir = root.join(".cadence").join("wt").join(&wt_name);

    // Recorded refs are matched by value, not first-of-kind: a
    // re-start under `--name` leaves the old refs in place (they are
    // history) and must not shadow the new ones.
    let wt_str = wt_dir.to_string_lossy().into_owned();
    let has_ref = |kind: &str, target: &str| {
        front
            .refs
            .iter()
            .any(|r| r.kind == kind && r.path.as_deref() == Some(target))
    };
    let first_recorded = |kind: &str| {
        front
            .refs
            .iter()
            .find(|r| r.kind == kind)
            .and_then(|r| r.path.clone())
    };
    let ours_recorded = has_ref("branch", &branch) && has_ref("worktree", &wt_str);
    // Clear registrations whose dirs are gone — a deleted worktree
    // must not block its own re-creation.
    let _ = git(&root, &["worktree", "prune"]);
    let dir_exists = wt_dir.is_dir();
    let branch_exists = git(
        &root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();

    let mut created = false;
    if ours_recorded && branch_exists {
        if !dir_exists {
            // Refs still accurate — re-attach the existing branch.
            worktree::add(&root, &wt_dir, None, &branch)?;
            worktree::ensure_cadence_ignored(&root)?;
        } else if worktree_branch(&wt_dir).as_deref() != Some(branch.as_str()) {
            return Err(Error::rejected(format!(
                "Worktree {} is checked out on '{}' — expected '{}'. \
                 Fix it or remove it before re-starting",
                wt_dir.display(),
                worktree_branch(&wt_dir).unwrap_or_else(|| "(detached)".to_string()),
                branch
            )));
        }
        // Idempotent: same issue, same names — no commit.
    } else {
        if branch_exists {
            return Err(Error::rejected(format!(
                "Branch '{branch}' already exists but the issue records \
                 worktree '{}' — pick --name or reconcile the refs first",
                first_recorded("worktree").unwrap_or_else(|| "(none)".to_string())
            )));
        }
        if dir_exists {
            return Err(Error::rejected(format!(
                "Worktree dir {} already exists but the issue records \
                 branch '{}' — remove it or pick --name",
                wt_dir.display(),
                first_recorded("branch").unwrap_or_else(|| "(none)".to_string())
            )));
        }
        worktree::add(&root, &wt_dir, Some(&branch), &base_sha)?;
        worktree::ensure_cadence_ignored(&root)?;
        created = true;

        let repo_label = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let mut new_front = front.clone();
        // Refs are pushed only when not already recorded — a front
        // saved-but-never-committed (crash, refused commit) must not
        // duplicate them on retry.
        if !has_ref("branch", &branch) {
            new_front.refs.push(Ref {
                kind: "branch".to_string(),
                url: None,
                path: Some(branch.clone()),
                label: Some(repo_label),
            });
        }
        if !has_ref("worktree", &wt_str) {
            new_front.refs.push(Ref {
                kind: "worktree".to_string(),
                url: None,
                path: Some(wt_str.clone()),
                label: None,
            });
        }
        if matches!(new_front.status.as_str(), "backlog" | "ready") {
            new_front.status = "doing".to_string();
        }
        if new_front.owner.is_none() {
            new_front.owner = Some(
                args.owner
                    .clone()
                    .unwrap_or_else(|| write::actor_who(actor, None)),
            );
        }
        // The worktree, the branch and the tracker commit stand or
        // fall together: a refused commit (hook lint, disk error)
        // rolls the file and the git side back so a retry is clean.
        let committed = write::save_front(&dir, &new_front, &body).and_then(|_| {
            write::commit(
                pm,
                &format!("{}: start {branch}", front.id),
                &[front.id.as_str()],
                actor,
            )
        });
        if let Err(e) = committed {
            let _ = write::save_front(&dir, &front, &body);
            let _ = git(
                &root,
                &["worktree", "remove", "--force", &wt_dir.to_string_lossy()],
            );
            let _ = git(&root, &["branch", "-D", &branch]);
            return Err(e);
        }
        front = new_front;
    }
    drop(_lock);

    let mut out = json!({
        "issue": front.id,
        "repo": root,
        "worktree": wt_dir,
        "branch": branch,
        "base": {"ref": base, "sha": base_sha},
        "trailer": format!("Issue: {}", front.id),
        "created": created,
    });
    if let (Some(job), Some((state_dir, spec, spec_sha256))) = (&args.job, job_probe) {
        let created_job = client::rpc(
            &state_dir,
            "job_new",
            json!({"pm": job.pm, "spec": spec, "spec_sha256": spec_sha256,
                   "title": front.title, "issue": front.id,
                   "repo": root, "base_ref": base_sha,
                   "task_worktree": wt_name, "task_branch": branch,
                   "task_base_sha": base_sha,
                   "task_assignee": job.assignee}),
        )?;
        let job_id = created_job["job"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        out["job"] = json!(job_id.clone());
        // `job_new` mints exactly one task — `<job>-t1` — scoped above.
        out["task"] = json!(format!("{job_id}-t1"));
    }
    Ok(out)
}
