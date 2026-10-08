//! `cadence issue start <ID>` — bind a project-repo worktree and
//! branch to an issue: mint `.cadence/wt/<id>-<slug>` on
//! `cadence/<id>-<slug>`, record both as refs, move `backlog|ready`
//! to `doing`, print the CAD-42 trailer. `--job` additionally opens
//! an M3 job whose task is already scoped to the worktree. An issue
//! with one open worktree ref reuses that lane (CAD-274): a re-start
//! re-applies the cargo target and mints nothing; a `--name` for a
//! different slug, or several open refs, refuses.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};
use crate::issue::model::{self, Front, Ref};
use crate::issue::{claim, finish, git, parse, project, write, Pm};
use crate::worktree::{self, layout};

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

/// Base resolution: an explicit `--base`, else fetch and pin the repo's
/// `origin/HEAD` target, else use the local branch (for repos without a remote
/// default). Returns the selected ref and its exact commit SHA.
pub(crate) fn resolve_base(root: &Path, flag: Option<&str>) -> Result<(String, String)> {
    let (base, fetch_default) = if let Some(b) = flag {
        (b.to_string(), false)
    } else if let Ok(origin_head) = git(
        root,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    ) {
        (origin_head, true)
    } else {
        (
            git(root, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_else(|_| "HEAD".to_string()),
            false,
        )
    };
    if fetch_default {
        let (remote, branch) = base.split_once('/').ok_or_else(|| {
            Error::rejected(format!("origin/HEAD resolved to invalid ref '{base}'"))
        })?;
        let refspec = format!("+refs/heads/{branch}:refs/remotes/{remote}/{branch}");
        git(
            root,
            &["fetch", "--no-tags", "--no-write-fetch-head", remote, &refspec],
        )
        .map_err(|e| {
            Error::rejected(format!(
                "Could not refresh default base '{base}' in {} ({e}) — retry when origin is reachable or pass --base <ref>",
                root.display()
            ))
        })?;
    }
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

fn branch_base(root: &Path, branch: &str, fallback: &str) -> String {
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

fn resolve_existing_base(root: &Path, branch: &str) -> Result<(String, String)> {
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
fn worktree_branch(dir: &Path) -> Result<Option<String>> {
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

fn refuse_terminal_lifecycle_record(
    repo: &Path,
    lane: &Path,
    front: &Front,
    branch: &str,
    branch_exists: bool,
) -> Result<()> {
    let Some(record) = worktree::lifecycle::managed_record(repo, lane)? else {
        return Ok(());
    };
    if matches!(record.state.as_str(), "retained" | "releasing") {
        let reason = record
            .retention_reason
            .as_deref()
            .or(record.release_reason.as_deref())
            .unwrap_or("no recorded reason");
        return Err(Error::rejected(format!(
            "lane {} is recorded as {} ({reason}); issue start will not implicitly reactivate it — inspect lifecycle inventory before explicit recovery",
            lane.display(), record.state
        )));
    }
    if record.state != "released" {
        return Ok(());
    }
    let refs_match = front
        .refs
        .iter()
        .any(|r| r.kind == "branch" && r.path.as_deref() == Some(branch))
        && front.refs.iter().any(|r| {
            r.kind == "worktree" && r.path.as_deref() == Some(lane.to_string_lossy().as_ref())
        });
    if record.purpose != "development"
        || record.tool != "cadence issue start"
        || record.issue.as_deref() != Some(front.id.as_str())
        || record.branch.as_deref() != Some(branch)
        || !refs_match
    {
        return Err(Error::rejected(format!(
            "released lane {} does not match this issue's recorded branch disposition; refusing restart",
            lane.display()
        )));
    }
    match std::fs::symlink_metadata(lane) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(Error::rejected(format!(
                "released lane {} still exists; issue start will not reactivate it",
                lane.display()
            )))
        }
        Err(error) => {
            return Err(Error::rejected(format!(
                "cannot verify released lane {} is absent: {error}",
                lane.display()
            )))
        }
    }
    let listed = git(repo, &["worktree", "list", "--porcelain"])?;
    let mut current = None;
    let mut target_registered = false;
    let mut branch_registered_elsewhere = false;
    for line in listed.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(path));
            if crate::issue::finish::lexical_path(Path::new(path))
                == crate::issue::finish::lexical_path(lane)
            {
                target_registered = true;
            }
        } else if line == format!("branch refs/heads/{branch}")
            && current.as_deref().is_some_and(|path| {
                crate::issue::finish::lexical_path(path) != crate::issue::finish::lexical_path(lane)
            })
        {
            branch_registered_elsewhere = true;
        }
    }
    if target_registered {
        return Err(Error::rejected(format!(
            "released lane {} remains registered by Git; refusing restart",
            lane.display()
        )));
    }
    if branch_exists && branch_registered_elsewhere {
        return Err(Error::rejected(format!(
            "released branch {branch} is checked out elsewhere; refusing restart"
        )));
    }
    Ok(())
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
/// exists, else the worktree layout walked upward ([`layout::root_of`]).
/// `None` when neither answers — the caller's resolved repo stands.
fn lane_root(lane: &Path) -> Option<PathBuf> {
    if lane.is_dir() {
        return worktree::main_root(lane).ok();
    }
    layout::root_of(lane)
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
    let slug = layout::issue_slug(&front.id, &wt_name);
    if let Some(name) = name {
        let (want, _) = layout::issue_names(&front.id, &front.title, Some(name))?;
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
    let named = layout::branch(&wt_name);
    let branch = if open_branch(&named) {
        named
    } else {
        worktree_branch(lane)?
            .filter(|b| open_branch(b))
            .unwrap_or(named)
    };
    Ok((lane.to_path_buf(), branch))
}

/// The lane `issue start` binds for `front` in repo `root` — `(dir,
/// branch)`. The issue's one open worktree ref when it has one (CAD-274,
/// [`reuse_lane`]), else a fresh [`layout::issue_names`] lane under
/// `root`; several open refs are ambiguous and refuse, naming them
/// instead of guessing. `dispatch` calls this same function to predict
/// the kickoff before `issue start` runs (CAD-388 R2-1), so the two can
/// never disagree about the names. Reads the tracker front and the
/// lane's checkout; creates nothing.
pub(crate) fn resolve_lane(
    front: &Front,
    name: Option<&str>,
    root: &Path,
) -> Result<(PathBuf, String)> {
    match open_worktrees(front).as_slice() {
        [] => {
            let (wt_name, branch) = layout::issue_names(&front.id, &front.title, name)?;
            Ok((layout::worktree_dir(root, wt_name), branch))
        }
        [lane] => reuse_lane(front, lane, name, root),
        many => Err(Error::rejected(format!(
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
        ))),
    }
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
    // CAD-360: the plan gate, before anything is created — `issue
    // start`, `dispatch` and `dispatch --job` all come through here.
    let (initial_front, initial_body) = write::load_front(&dir)?;
    crate::issue::plan::gate(&pm.dir, &initial_front, &initial_body)?;
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
    let (initial_lane, initial_branch) = resolve_lane(&initial_front, args.name.as_deref(), &root)?;
    let initial_open = open_worktrees(&initial_front);
    let initial_branch_exists = git(
        &root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{initial_branch}"),
        ],
    )
    .is_ok();
    refuse_terminal_lifecycle_record(
        &root,
        &initial_lane,
        &initial_front,
        &initial_branch,
        initial_branch_exists,
    )?;
    let initial_lane_str = initial_lane.to_string_lossy().into_owned();
    let initial_refs_recorded = initial_front
        .refs
        .iter()
        .any(|r| r.kind == "branch" && r.path.as_deref() == Some(initial_branch.as_str()))
        && initial_front
            .refs
            .iter()
            .any(|r| r.kind == "worktree" && r.path.as_deref() == Some(initial_lane_str.as_str()));
    let initial_lifecycle_record = worktree::lifecycle::recoverable_record(
        &root,
        &initial_lane,
        "development",
        "cadence issue start",
        Some(&initial_branch),
        Some(&initial_front.id),
    )?;
    let initial_reuse = initial_branch_exists
        && (!initial_open.is_empty()
            || initial_refs_recorded
            || initial_lifecycle_record.is_some());
    let (base, base_sha) = if initial_reuse && args.base.is_none() {
        resolve_existing_base(&root, &initial_branch)?
    } else {
        resolve_base(&root, args.base.as_deref())?
    };
    let shared_deps = worktree::shared_deps_enabled(&project)?;

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

    // CAD-1191: lock 1 (short) — claim check, take-over and, for a new
    // lane, the claim record. Worktree work runs unlocked; lock 2
    // records the lane refs.
    let lock = pm.lock()?;
    let (mut front, body) = write::load_front(&dir)?;
    let front_id = front.id.clone();
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
            pm, &dir, &front, &taken, &body, &requester, "claim", &text, &subject, actor,
        )?;
        front = taken;
    }
    // CAD-274: an open worktree ref IS the issue's lane — a re-start
    // reuses it (re-applying the cargo target) rather than minting a
    // second lane from the title.
    let open = open_worktrees(&front);
    let (wt_dir, branch) = resolve_lane(&front, args.name.as_deref(), &root)?;
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
    refuse_terminal_lifecycle_record(&root, &wt_dir, &front, &branch, branch_exists)?;
    let lifecycle_record = worktree::lifecycle::recoverable_record(
        &root,
        &wt_dir,
        "development",
        "cadence issue start",
        Some(&branch),
        Some(&front.id),
    )?;
    let reuse = branch_exists
        && (!open.is_empty()
            || recorded("branch", &branch) && recorded("worktree", &wt_str)
            || lifecycle_record.is_some());
    if initial_reuse && !reuse && args.base.is_none() {
        return Err(Error::rejected(
            "the recorded lane changed while issue start was waiting; retry to refresh and pin the default base",
        ));
    }
    let repo_label = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let checkout_owner = front
        .owner
        .clone()
        .or_else(|| new_owner.clone())
        .unwrap_or_else(|| requester.clone());
    let pinned_base = if branch_exists {
        branch_base(&root, &branch, &base_sha)
    } else {
        base_sha.clone()
    };
    let lane_base_sha = if reuse {
        pinned_base.clone()
    } else {
        base_sha.clone()
    };
    let checkout_record = || {
        let mut record = worktree::lifecycle::new_record(worktree::lifecycle::CheckoutSpec {
            repo: &root,
            purpose: "development",
            tool: "cadence issue start",
            owner: &checkout_owner,
            path: &wt_dir,
            branch: Some(&branch),
            pinned_sha: &pinned_base,
            issue: Some(&front_id),
        });
        record.base_sha = Some(pinned_base.clone());
        record
    };

    // A new lane is validated, then its claim is recorded under this
    // lock — the commit that makes a concurrent start refuse — before
    // any worktree work. (A reused lane keeps today's rule: no claim
    // commit; lock 2 only refreshes refs.)
    let mut claim_at: Option<String> = None;
    let mut pre_claim: Option<(String, Option<String>, Option<model::Claim>)> = None;
    if !reuse {
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
        let mut claimed = front.clone();
        if matches!(claimed.status.as_str(), "backlog" | "ready") {
            claimed.status = "doing".to_string();
        }
        if claimed.owner.is_none() {
            claimed.owner = Some(
                new_owner
                    .clone()
                    .unwrap_or_else(|| write::actor_who(actor, args.by.as_deref())),
            );
        }
        // CAD-383: the start that puts the issue into work claims it —
        // the requester (the dispatching PM), not the lane.
        if claimed.claim.is_none() || checked.warning.is_some() {
            claimed.claim = Some(claim::new_claim(&requester, None));
        }
        claim_at = claimed.claim.as_ref().map(|c| c.at.clone());
        if claimed.status != front.status
            || claimed.owner != front.owner
            || claimed.claim != front.claim
        {
            let committed = write::save_front(&dir, &claimed, &body).and_then(|_| {
                write::commit_who(
                    pm,
                    &[dir.join("issue.md")],
                    &format!("{}: start {branch} (claim)", front.id),
                    &[front.id.as_str()],
                    actor,
                    Some(&requester),
                )
            });
            if let Err(e) = committed {
                let _ = write::save_front(&dir, &front, &body);
                return Err(e);
            }
            pre_claim = Some((
                front.status.clone(),
                front.owner.clone(),
                front.claim.clone(),
            ));
        }
    }
    drop(lock);

    // Unlocked: worktree creation and setup (cargo target, hooks). The
    // lifecycle ledger records the intent first, so a crash here leaves
    // an inventoried `preparing` checkout that a re-run reuses.
    crate::issue::lockseam::outside_lock("start-worktree");
    // Clear registrations whose dirs are gone — a deleted worktree
    // must not block its own re-creation.
    let _ = git(&root, &["worktree", "prune"]);
    let setup = || -> Result<(Option<PathBuf>, bool)> {
        let mut created = false;
        let cargo_target: Option<PathBuf>;
        if reuse {
            let reattached = !dir_exists;
            if reattached {
                // Refs still accurate — record the interrupted recovery before
                // re-attaching the surviving branch.
                worktree::ensure_cadence_ignored(&root)?;
                worktree::lifecycle::begin(&root, checkout_record())?;
                if let Err(e) = worktree::add(&root, &wt_dir, None, &branch) {
                    let _ = worktree::lifecycle::transition(
                        &root,
                        &wt_dir,
                        "setup-failed",
                        Some(&e.to_string()),
                    );
                    return Err(e);
                }
            } else {
                worktree::validate_registered_branch(&root, &wt_dir, &branch)?;
            }
            cargo_target = match worktree::setup_development(&wt_dir, &root, shared_deps) {
                Ok(target) => target,
                Err(e) => {
                    let _ = worktree::lifecycle::transition(
                        &root,
                        &wt_dir,
                        "setup-failed",
                        Some(&e.to_string()),
                    );
                    return Err(e);
                }
            };
            // Reused lanes re-heal shared cargo setup and the per-worktree hook.
            if let Err(e) = worktree::lifecycle::activate(&root, checkout_record()) {
                let _ = worktree::lifecycle::transition(
                    &root,
                    &wt_dir,
                    "setup-failed",
                    Some(&e.to_string()),
                );
                return Err(e);
            }
        } else {
            // Persist intent first: if the process stops during setup, inventory
            // reports an interrupted checkout instead of inferring ownership from
            // its directory name.
            worktree::ensure_cadence_ignored(&root)?;
            worktree::lifecycle::begin(&root, checkout_record())?;
            if let Err(e) = worktree::add(&root, &wt_dir, Some(&branch), &base_sha) {
                let _ = worktree::lifecycle::transition(
                    &root,
                    &wt_dir,
                    "setup-failed",
                    Some(&e.to_string()),
                );
                return Err(e);
            }
            // Existing build-target and pre-push setup must succeed before the
            // checkout becomes active in the tracker.
            cargo_target = match worktree::setup_development(&wt_dir, &root, shared_deps) {
                Ok(target) => target,
                Err(e) => {
                    let _ = worktree::lifecycle::transition(
                        &root,
                        &wt_dir,
                        "setup-failed",
                        Some(&e.to_string()),
                    );
                    return Err(e);
                }
            };
            if let Err(e) = worktree::lifecycle::activate(&root, checkout_record()) {
                let _ = worktree::lifecycle::transition(
                    &root,
                    &wt_dir,
                    "setup-failed",
                    Some(&e.to_string()),
                );
                return Err(e);
            }
            created = true;
        }
        Ok((cargo_target, created))
    };
    let (cargo_target, created) = match setup() {
        Ok(done) => done,
        Err(e) => {
            // A refused new lane leaves the issue as it was: give the
            // claim back (only if it is still the one lock 1 recorded).
            if let (Some((status, owner, prev_claim)), Some(at)) = (pre_claim, &claim_at) {
                let _lock = pm.lock()?;
                let (mut cur, cur_body) = write::load_front(&dir)?;
                if cur
                    .claim
                    .as_ref()
                    .is_some_and(|c| c.by == requester && &c.at == at)
                {
                    cur.status = status;
                    cur.owner = owner;
                    cur.claim = prev_claim;
                    let undone = write::save_front(&dir, &cur, &cur_body).and_then(|_| {
                        write::commit_who(
                            pm,
                            &[dir.join("issue.md")],
                            &format!("{}: start {branch} (claim released: setup failed)", cur.id),
                            &[cur.id.as_str()],
                            actor,
                            Some(&requester),
                        )
                    });
                    if undone.is_err() {
                        let _ = write::save_front(&dir, &front, &cur_body);
                    }
                }
            }
            return Err(e);
        }
    };

    // Lock 2 (short): re-load the issue and prove the claim is still
    // ours before recording the lane. The refs are merged onto the fresh
    // front, so an unrelated edit made meanwhile (a comment, a priority
    // change) is kept, never overwritten and never a reason to refuse.
    let lock = pm.lock()?;
    let (cur, cur_body) = write::load_front(&dir)?;
    let claim_lost = |why: String| {
        Error::rejected(format!(
            "{}: {why} while the lane was being set up — refs for {branch} were not \
             recorded. The worktree {} and its lifecycle record are left in place; \
             re-run `cadence issue start {}` to reuse the lane once the claim is yours",
            cur.id,
            wt_dir.display(),
            cur.id
        ))
    };
    if created || claim_at.is_some() {
        // A new lane: the claim lock 1 recorded must still be ours.
        let ours = cur
            .claim
            .as_ref()
            .is_some_and(|c| c.by == requester && Some(&c.at) == claim_at.as_ref());
        if !ours {
            return Err(claim_lost(format!(
                "the claim on it changed (now held by {})",
                claim::holders(&cur).join(", ")
            )));
        }
    } else {
        // A reused lane: the same claim check lock 1 ran, with no
        // take-over available — only a holder that is not us refuses.
        let mut asking = vec![requester.as_str()];
        asking.extend(new_owner.as_deref());
        match claim::check(&cur, &asking, None, "issue start", || None) {
            Ok(c) if c.take_over.is_none() => {}
            _ => {
                return Err(claim_lost(format!(
                    "it was claimed by {}",
                    claim::holders(&cur).join(", ")
                )))
            }
        }
    }
    let body = cur_body;
    // Refs already recorded (a front saved-but-never-committed on a
    // crash, or a closed pair of the same names) are re-opened, never
    // duplicated. Idempotent: the same lane commits nothing, unless its
    // refs need a fix (a stale cargo target, a closed pair re-opened, a
    // missing half of the pair).
    let refreshed = with_lane_refs(&cur, &branch, &wt_str, &repo_label, &cargo_target);
    front = cur;
    if refreshed.refs != front.refs {
        // `Actor:` is the requester (the dispatching PM), so the lane's
        // advisory code-area PM is bound to this record and not to live
        // frontmatter an agent can rewrite.
        let subject = if created {
            format!("{}: start {branch}", front.id)
        } else {
            format!("{}: start {branch} (refs refreshed)", front.id)
        };
        let committed = write::save_front(&dir, &refreshed, &body).and_then(|_| {
            write::commit_who(
                pm,
                &[dir.join("issue.md")],
                &subject,
                &[front.id.as_str()],
                actor,
                Some(&requester),
            )
        });
        if let Err(e) = committed {
            let _ = write::save_front(&dir, &front, &body);
            if created {
                // The managed checkout stays in setup-failed state for an
                // explicit retry.
                let _ = worktree::lifecycle::transition(
                    &root,
                    &wt_dir,
                    "setup-failed",
                    Some(&e.to_string()),
                );
            }
            return Err(e);
        }
        front = refreshed;
    }
    drop(lock);

    // CAD-113: the worktree's slot environment — a write failure is
    // reported, never silently swallowed.
    let slot_env = match write_slot_env(&wt_dir, &pm.dir) {
        Ok(path) => json!({"path": path}),
        Err(e) => {
            let _ = worktree::lifecycle::transition(
                &root,
                &wt_dir,
                "setup-failed",
                Some(&e.to_string()),
            );
            json!({"error": e.to_string(), "recovery": format!("retry issue start to reapply setup: {e}")})
        }
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
        "base": {"ref": base, "sha": lane_base_sha},
        "trailer": format!("Issue: {}", front.id),
        "created": created,
        "target_dir": cargo_target,
        "slot_env": slot_env,
        "claim": claim_out,
    });
    if let (Some(job), Some((state_dir, spec, spec_sha256))) = (&args.job, job_probe) {
        // CAD-159: the issue's acceptance items ride the scoped task,
        // so the daemon's `job dispatch` kickoff lists them — all of
        // them, never a pointer (CAD-160): the kickoff refuses a list
        // that cannot fit rather than dropping it.
        let task_acceptance =
            crate::issue::dispatch::acceptance_listing(&parse::acceptance_items(&body));
        let created_job = client::rpc(
            &state_dir,
            "job_new",
            json!({"pm": job.pm, "spec": spec, "spec_sha256": spec_sha256,
                   "title": front.title, "issue": front.id,
                   "repo": root, "base_ref": lane_base_sha,
                   "task_worktree": wt_name, "task_branch": branch,
                   "task_base_sha": lane_base_sha,
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
    // CAD-378: advisory path leases — the ticket's planned paths
    // against the project's code areas and the other open lanes. Never
    // refuses: the warnings ride the output, and a start that minted
    // the lane records them on the issue as a `lease` comment (the
    // event). A failure to record is reported, not raised.
    let mut leases =
        crate::issue::areas::check_start(&pm.dir, state_dir, &project.key, &front, &requester);
    let lines = crate::issue::areas::warning_lines(&leases);
    if created && !lines.is_empty() {
        let text = format!("Lease warnings at start:\n- {}", lines.join("\n- "));
        if let Err(e) = write::add_comment(pm, id, &text, None, Some("lease"), None, actor) {
            leases["record_error"] = json!(e.to_string());
        }
    }
    out["leases"] = leases;
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
