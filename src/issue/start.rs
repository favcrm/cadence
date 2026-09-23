//! `cadence issue start <ID>` — bind a project-repo worktree and
//! branch to an issue: mint `.cadence/wt/<id>-<slug>` on
//! `cadence/<id>-<slug>`, record both as refs, move `backlog|ready`
//! to `doing`, print the CAD-42 trailer. `--job` additionally opens
//! an M3 job whose task is already scoped to the worktree. An issue
//! with one open worktree ref reuses that lane (CAD-274): a re-start
//! re-applies the cargo target and mints nothing; a `--name` for a
//! different slug, or several open refs, refuses.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};
use crate::issue::model::{self, Front, Ref};
use crate::issue::{claim, git, parse, project, write, Pm};
use crate::{proto, worktree};

/// Optional M3 job creation: `--job --pm <alias> --spec <file>
/// [--assignee <alias>]`.
pub struct JobArgs {
    pub pm: String,
    pub spec: PathBuf,
    pub assignee: Option<String>,
    /// CAD-202 `--force`: bind an assignee whose pty pane cwd is
    /// outside the project's repos; the override is recorded.
    pub force: bool,
}

pub struct StartArgs {
    pub repo: Option<PathBuf>,
    pub name: Option<String>,
    pub base: Option<String>,
    pub owner: Option<String>,
    pub job: Option<JobArgs>,
    /// CAD-383: who is asking — `--by`, else the job's `--pm`, else
    /// `CADENCE_ALIAS`, else `operator`. `dispatch` passes its
    /// `--reply-to`.
    pub by: Option<String>,
    /// CAD-383 `--take-over <reason>`: start an issue someone else holds
    /// in doing/review; recorded on the issue.
    pub take_over: Option<String>,
}

/// ASCII-lower `-`-separated slug, ≤32 chars — `New Login Form` →
/// `new-login-form`. Non-ASCII titles fall back to `work`.
pub(crate) fn slugify(title: &str) -> String {
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

/// Derive the worktree name and branch for an issue start — shared by
/// `dispatch`, which must know the names before anything is created.
/// Returns `(wt_name, branch)`; the dir is `<root>/.cadence/wt/<wt_name>`.
pub(crate) fn names(id: &str, title: &str, name: Option<&str>) -> Result<(String, String)> {
    let slug = match name {
        Some(name) => proto::identifier(name, "--name")?,
        None => slugify(title),
    };
    let wt_name = format!("{}-{}", id.to_lowercase(), slug);
    proto::identifier(&wt_name, "Worktree name")?;
    Ok((wt_name.clone(), format!("cadence/{wt_name}")))
}

/// The project's declared repo roots, canonicalized.
pub(crate) fn declared_repos(project: &project::Project) -> Vec<PathBuf> {
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
    Err(Error::rejected(format!(
        "Which repo does this issue work in? Pass --repo — {}",
        repo_list(project, &candidates)
    )))
}

/// Base resolution per decision 1: `--base`, then the repo's
/// `origin/HEAD` target, then the current branch, then `HEAD`.
/// Returns `(ref, sha)`; no fetch.
pub(crate) fn resolve_base(root: &Path, flag: Option<&str>) -> Result<(String, String)> {
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

/// The issue's open worktree refs, in recorded order.
pub(crate) fn open_worktrees(front: &Front) -> Vec<PathBuf> {
    front
        .refs
        .iter()
        .filter(|r| r.kind == "worktree" && r.closed != Some(true))
        .filter_map(|r| r.path.as_deref().map(PathBuf::from))
        .collect()
}

/// The repo a recorded lane belongs to: through the checkout when it
/// exists, else the `<root>/.cadence/wt/<name>` layout walked upward.
/// `None` when neither answers — the caller's resolved repo stands.
fn lane_root(lane: &Path) -> Option<PathBuf> {
    if lane.is_dir() {
        return worktree::main_root(lane).ok();
    }
    let wt = lane.parent()?;
    let cadence = wt.parent()?;
    if wt.file_name()? != "wt" || cadence.file_name()? != ".cadence" {
        return None;
    }
    cadence.parent()?.canonicalize().ok()
}

/// CAD-274: the one open lane an issue already has — `(dir, branch)`.
/// A `--name` naming a different slug would fork the issue's work into
/// a second lane and is refused, naming the lane that exists; so is a
/// lane recorded in another repo than the one resolved. The branch is
/// the open branch ref `cadence/<dir name>`, else the branch the dir
/// has checked out when that is recorded open (a moved lane), else
/// `cadence/<dir name>`.
fn reuse_lane(
    front: &Front,
    lane: &Path,
    name: Option<&str>,
    root: &Path,
) -> Result<(PathBuf, String)> {
    let wt_name = lane
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = format!("{}-", front.id.to_lowercase());
    let slug = wt_name.strip_prefix(&prefix).unwrap_or(&wt_name);
    if let Some(name) = name {
        let (want, _) = names(&front.id, &front.title, Some(name))?;
        if want != wt_name {
            return Err(Error::rejected(format!(
                "{id} already has an open worktree '{slug}' at {path} — \
                 --name {name} would start a second lane. Re-run without \
                 --name to reuse it, or close it first with `cadence issue \
                 finish {id} --worktree {path}`",
                id = front.id,
                path = lane.display()
            )));
        }
    }
    if let Some(other) = lane_root(lane).filter(|r| r != root) {
        return Err(Error::rejected(format!(
            "{}'s open worktree {} belongs to repo {}, not {} — pass --repo {}",
            front.id,
            lane.display(),
            other.display(),
            root.display(),
            other.display()
        )));
    }
    let open_branch = |b: &str| {
        front
            .refs
            .iter()
            .any(|r| r.kind == "branch" && r.closed != Some(true) && r.path.as_deref() == Some(b))
    };
    let named = format!("cadence/{wt_name}");
    let branch = if open_branch(&named) {
        named
    } else {
        worktree_branch(lane)
            .filter(|b| open_branch(b))
            .unwrap_or(named)
    };
    Ok((lane.to_path_buf(), branch))
}

/// `front` with the lane's branch and worktree refs recorded open —
/// an existing ref of the same value is re-opened (never duplicated),
/// else one is added — and the worktree ref's cargo target current.
fn with_lane_refs(
    front: &Front,
    branch: &str,
    wt: &str,
    repo_label: &str,
    cargo_target: &Option<PathBuf>,
) -> Front {
    fn open_ref<'a>(refs: &'a mut Vec<Ref>, kind: &str, path: &str, label: &str) -> &'a mut Ref {
        let same = |r: &Ref| r.kind == kind && r.path.as_deref() == Some(path);
        let at = refs
            .iter()
            .position(|r| same(r) && r.closed != Some(true))
            .or_else(|| refs.iter().rposition(same));
        let at = at.unwrap_or_else(|| {
            refs.push(Ref {
                kind: kind.to_string(),
                url: None,
                path: Some(path.to_string()),
                label: (!label.is_empty()).then(|| label.to_string()),
                closed: None,
                worktree: None,
                cargo_target: None,
                agent: None,
            });
            refs.len() - 1
        });
        let r = &mut refs[at];
        if r.closed == Some(true) {
            r.closed = None;
        }
        r
    }
    let mut out = front.clone();
    open_ref(&mut out.refs, "branch", branch, repo_label);
    open_ref(&mut out.refs, "worktree", wt, "").cargo_target = cargo_target
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());
    out
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
    // CAD-202: a pty assignee whose pane cwd is deleted or outside the
    // project's repos refuses here, before anything is created.
    let cwd_override = match &args.job {
        Some(JobArgs {
            assignee: Some(assignee),
            force,
            ..
        }) => {
            let show = client::rpc(state_dir, "agent_show", json!({"alias": assignee}))?;
            crate::issue::dispatch::check_lane_cwd(&project, assignee, &show["agent"], *force)?
        }
        _ => None,
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = resolve_repo(&project, args.repo.as_deref(), &cwd)?;
    let (base, base_sha) = resolve_base(&root, args.base.as_deref())?;

    // CAD-383: who is asking, and the owner this start would record.
    if let Some(by) = &args.by {
        claim::check_alias(by, "--by")?;
    }
    let requester = write::actor_who(
        actor,
        args.by
            .as_deref()
            .or(args.job.as_ref().map(|j| j.pm.as_str())),
    );
    let new_owner = args
        .owner
        .clone()
        .or_else(|| args.job.as_ref().and_then(|j| j.assignee.clone()));

    let _lock = pm.lock()?;
    let (mut front, body) = write::load_front(&dir)?;
    // CAD-383: a doing/review issue held by someone else refuses before
    // any lane is touched; a take-over is its own commit, made now.
    let mut asking = vec![requester.as_str()];
    asking.extend(new_owner.as_deref());
    let checked = claim::check(
        &front,
        &asking,
        args.take_over.as_deref(),
        "issue start",
        || claim::since(&pm.dir, &project.key, &front, Duration::from_secs(2)),
    )?;
    if let Some(t) = &checked.take_over {
        let mut taken = front.clone();
        taken.claim = Some(claim::new_claim(&requester, Some(t.reason.clone())));
        taken.owner = Some(new_owner.clone().unwrap_or_else(|| requester.clone()));
        let (text, subject) = claim::take_over_record(&requester, t);
        write::commit_front_with_comment(
            pm, &dir, &front, &taken, &body, &requester, &text, &subject, actor,
        )?;
        front = taken;
    }
    // CAD-274: an open worktree ref IS the issue's lane — a re-start
    // reuses it (re-applying the cargo target) rather than minting a
    // second lane from the title. Several open refs are ambiguous:
    // refuse and name them instead of guessing.
    let open = open_worktrees(&front);
    let (wt_dir, branch) = match open.as_slice() {
        [] => {
            let (wt_name, branch) = names(&front.id, &front.title, args.name.as_deref())?;
            (root.join(".cadence").join("wt").join(wt_name), branch)
        }
        [lane] => reuse_lane(&front, lane, args.name.as_deref(), &root)?,
        many => {
            return Err(Error::rejected(format!(
                "{} has {} open worktree refs — refusing to guess which lane \
                 to start:\n  {}\nClose the stale ones with `cadence issue \
                 finish {} --worktree <path>`",
                front.id,
                many.len(),
                many.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n  "),
                front.id
            )))
        }
    };
    // A reused lane's values come from the tracker and the checkout —
    // never re-record one git would read as an option (CAD-144).
    model::check_ref_value(&branch)?;
    model::check_ref_value(&wt_dir.to_string_lossy())?;
    let wt_name = wt_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Recorded refs are matched by value, not first-of-kind: a lane
    // finished earlier leaves its closed refs as history, and a
    // re-start under the same names re-opens them.
    let wt_str = wt_dir.to_string_lossy().into_owned();
    let recorded = |kind: &str, target: &str| {
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
    let reuse = branch_exists
        && (!open.is_empty() || recorded("branch", &branch) && recorded("worktree", &wt_str));
    let repo_label = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());

    let mut created = false;
    let cargo_target: Option<PathBuf>;
    if reuse {
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
        cargo_target = worktree::configure_cargo_target(
            &wt_dir,
            &root,
            worktree::shared_deps_enabled(&project)?,
        )?;
        // Idempotent: same lane — no commit, unless its refs need a
        // fix: a stale recorded cargo target (project config flipped,
        // an older cadence recorded a different layout), a closed
        // pair re-opened, a missing half of the pair. The fix is a
        // real commit, same as any ref edit.
        let refreshed = with_lane_refs(&front, &branch, &wt_str, &repo_label, &cargo_target);
        if refreshed.refs != front.refs {
            let committed = write::save_front(&dir, &refreshed, &body).and_then(|_| {
                write::commit(
                    pm,
                    &format!("{}: start {branch} (refs refreshed)", front.id),
                    &[front.id.as_str()],
                    actor,
                )
            });
            if let Err(e) = committed {
                let _ = write::save_front(&dir, &front, &body);
                return Err(e);
            }
            front = refreshed;
        }
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
        // A failed target setup leaves the lane behind — roll the git
        // side back so a retry starts clean.
        match worktree::configure_cargo_target(
            &wt_dir,
            &root,
            worktree::shared_deps_enabled(&project)?,
        ) {
            Ok(target) => cargo_target = target,
            Err(e) => {
                let _ = git(
                    &root,
                    &["worktree", "remove", "--force", &wt_dir.to_string_lossy()],
                );
                let _ = git(&root, &["branch", "-D", &branch]);
                return Err(e);
            }
        }
        created = true;

        // Refs already recorded (a front saved-but-never-committed on
        // a crash or refused commit, or a closed pair of the same
        // names) are re-opened, never duplicated.
        let mut new_front = with_lane_refs(&front, &branch, &wt_str, &repo_label, &cargo_target);
        if matches!(new_front.status.as_str(), "backlog" | "ready") {
            new_front.status = "doing".to_string();
        }
        if new_front.owner.is_none() {
            new_front.owner = Some(
                new_owner
                    .clone()
                    .unwrap_or_else(|| write::actor_who(actor, args.by.as_deref())),
            );
        }
        // CAD-383: the start that puts the issue into work claims it —
        // the requester (the dispatching PM), not the lane.
        if new_front.claim.is_none() || checked.warning.is_some() {
            new_front.claim = Some(claim::new_claim(&requester, None));
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

    // CAD-113: the worktree's slot environment — a write failure is
    // reported, never silently swallowed.
    let slot_env = match write_slot_env(&wt_dir, &pm.dir) {
        Ok(path) => json!({"path": path}),
        Err(e) => json!({"error": e.to_string()}),
    };
    let mut claim_out = claim::json(&front, crate::issue::time::now_epoch());
    claim_out["warning"] = json!(checked.warning);
    claim_out["take_over"] = json!(checked
        .take_over
        .as_ref()
        .map(|t| json!({"from": t.from, "reason": t.reason})));
    let mut out = json!({
        "issue": front.id,
        "repo": root,
        "worktree": wt_dir,
        "branch": branch,
        "base": {"ref": base, "sha": base_sha},
        "trailer": format!("Issue: {}", front.id),
        "created": created,
        "target_dir": cargo_target,
        "slot_env": slot_env,
        "claim": claim_out,
    });
    if let (Some(job), Some((state_dir, spec, spec_sha256))) = (&args.job, job_probe) {
        // CAD-159: the issue's acceptance items ride the scoped task,
        // so the daemon's `job dispatch` kickoff lists them.
        let task_acceptance = crate::issue::dispatch::acceptance_listing(
            &front.id,
            &parse::acceptance_items(&body),
            crate::issue::dispatch::JOB_ACCEPTANCE_BUDGET,
        );
        let created_job = client::rpc(
            &state_dir,
            "job_new",
            json!({"pm": job.pm, "spec": spec, "spec_sha256": spec_sha256,
                   "title": front.title, "issue": front.id,
                   "repo": root, "base_ref": base_sha,
                   "task_worktree": wt_name, "task_branch": branch,
                   "task_base_sha": base_sha,
                   "task_assignee": job.assignee,
                   "task_acceptance": task_acceptance}),
        )?;
        let job_id = created_job["job"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        out["job"] = json!(job_id.clone());
        // `job_new` mints exactly one task — `<job>-t1` — scoped above.
        out["task"] = json!(format!("{job_id}-t1"));
    }
    if let Some(note) = cwd_override {
        // The override is part of the issue's record, not just output.
        write::add_comment(pm, id, &note, None, Some("dispatch"), None, actor)?;
        out["cwd_override"] = json!(note);
    }
    Ok(out)
}

/// CAD-113: the worktree's build-slot environment — `CARGO_BUILD_JOBS`
/// from `[host] jobs_per_lane` (default 4) and the `build-slot` helper
/// path, so a worker never has to remember flags. Idempotent: lines we
/// own are rewritten, everything else in an existing `.env` survives.
/// Atomic (tmp + rename — a reader never sees a torn file), refuses to
/// write through a symlink, keeps an existing file's mode and creates
/// 0600.
fn write_slot_env(wt_dir: &Path, pm_dir: &Path) -> Result<PathBuf> {
    // The generated env must never dirty the worktree — `issue
    // finish`'s clean-tree guard reads `git status`. `.git/info/
    // exclude` covers untracked `.env` without touching tracked files
    // (a linked worktree resolves this to the COMMON git dir, so the
    // anchored `/.env` entry applies at every worktree's root and the
    // main checkout's — permanently; see docs/BOARD.md). The flock
    // serializes concurrent `issue start`s: check-and-append under
    // LOCK_EX can never double-write.
    let mut exclude = PathBuf::from(git(wt_dir, &["rev-parse", "--git-path", "info/exclude"])?);
    if exclude.is_relative() {
        exclude = wt_dir.join(exclude);
    }
    if let Some(dir) = exclude.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(&exclude)?;
    {
        use std::io::Read;
        use std::os::unix::io::AsRawFd;
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(Error::internal(format!(
                "flock {}: {}",
                exclude.display(),
                std::io::Error::last_os_error()
            )));
        }
        let mut text = String::new();
        f.read_to_string(&mut text)?;
        let covered: Vec<&str> = text.lines().map(|l| l.trim()).collect();
        // Anchored to the worktree root — a bare `.env` would hide
        // `.env` at ANY depth in EVERY worktree forever. An existing
        // unanchored entry still counts as coverage (superset), so a
        // host that ran the older writer never gains a duplicate.
        for p in ["/.env", "/.env.tmp"] {
            let bare = &p[1..];
            if !covered.iter().any(|l| *l == p || *l == bare) {
                writeln!(f, "{p}")?;
            }
        }
        // flock releases with the fd on drop.
    }
    let jobs = crate::doctor::host::host_overrides(pm_dir)
        .and_then(|o| o.jobs_per_lane)
        .unwrap_or(4);
    let helper = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cadence"));
    let file = wt_dir.join(".env");
    let owned = ["CARGO_BUILD_JOBS=", "CADENCE_BUILD_SLOT="];
    let mut text = String::new();
    let mut mode = 0o600;
    if let Ok(meta) = std::fs::symlink_metadata(&file) {
        if meta.file_type().is_symlink() {
            return Err(Error::rejected(format!(
                "{} is a symlink — refusing to write the slot env through it",
                file.display()
            )));
        }
        mode = meta.permissions().mode() & 0o777;
        if let Ok(existing) = std::fs::read_to_string(&file) {
            for line in existing.lines() {
                if !owned.iter().any(|p| line.starts_with(p)) {
                    text.push_str(line);
                    text.push('\n');
                }
            }
        }
    }
    text.push_str(&format!("CARGO_BUILD_JOBS={jobs}\n"));
    text.push_str(&format!("CADENCE_BUILD_SLOT={}\n", helper.display()));
    // `.env.tmp` sits beside the target and is excluded above too, so
    // even a SIGKILL mid-write can never leave a dirty worktree.
    let tmp = wt_dir.join(".env.tmp");
    let write = || -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        std::fs::rename(&tmp, &file)?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(file)
}
