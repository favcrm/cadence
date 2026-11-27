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
use crate::issue::{claim, git, parse, write, Pm};
use crate::worktree::{self, layout};

mod base;

pub use base::declared_repos;
use base::{branch_base, resolve_existing_base, worktree_branch};
#[doc(hidden)]
pub use base::{configured_integration, intake_branch_creation_base, resolve_base, resolve_repo};

/// Optional M3 job creation: `--job --pm <alias> --spec <file>
/// [--assignee <alias>]`.
pub struct JobArgs {
    /// Native client key when dispatch has proved an existing draft binding.
    /// `None` creates a fresh job through the unchanged one-open-job guard.
    pub id: Option<String>,
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
pub fn open_worktrees(front: &Front) -> Vec<PathBuf> {
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
                "{id} already has an open worktree {slug} at {path} — \
                 --name {name} would start a second lane. Re-run without \
                 --name to reuse it, or close it first with {finish}",
                id = front.id,
                slug = crate::printed_command::sanitized(slug),
                path = crate::printed_command::shown(lane),
                finish = {
                    use crate::printed_command::{command, Part};
                    command(&[
                        Part::Fixed("cadence issue finish "),
                        Part::Value(&front.id),
                        Part::Fixed(" --worktree "),
                        Part::Path(lane),
                    ])
                },
            )));
        }
    }
    if let Some(other) = lane_root(lane).filter(|r| r != root) {
        return Err(Error::rejected(format!(
            "{}: open worktree {} belongs to repo {}, not {} — pass --repo {}",
            front.id,
            crate::printed_command::shown(lane),
            crate::printed_command::shown(&other),
            crate::printed_command::shown(root),
            crate::printed_command::shown(&other)
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
                .map(|p| crate::printed_command::shown(p))
                .collect::<Vec<_>>()
                .join("\n  "),
            front.id
        ))),
    }
}
/// A sandbox has a branch, not a host worktree. A replacement job keeps
/// the sole open branch; branch history does not depend on the issue title.
pub(super) fn resolve_cloud_branch(front: &Front, name: Option<&str>) -> Result<(String, bool)> {
    let Some(branch) = crate::issue::cloud::open_lane_branch(front)? else {
        return layout::issue_names(&front.id, &front.title, name)
            .map(|(_, branch)| (branch, false));
    };
    if let Some(name) = name {
        let (_, wanted) = layout::issue_names(&front.id, &front.title, Some(name))?;
        if wanted != branch {
            return Err(Error::rejected(format!(
                "{} already has an open cloud branch {branch}; --name {name} would start another lane",
                front.id
            )));
        }
    }
    Ok((branch.to_string(), true))
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
    run_with_verified_job(pm, id, args, actor, state_dir, None)
}

struct StartRequest<'a> {
    pm: &'a Pm,
    id: &'a str,
    args: &'a StartArgs,
    actor: &'a str,
    state_dir: &'a Path,
    verified_job: Option<&'a Value>,
}

struct StartActors {
    requester: String,
    new_owner: Option<String>,
}

struct PreClaimContext {
    project: crate::issue::project::Project,
    dir: PathBuf,
    initial_front: Front,
    initial_intake: Option<String>,
    job_probe: Option<(PathBuf, String, String)>,
    cloud: bool,
    cwd_override: Option<String>,
}

struct InitialBaseContext<'a> {
    root: PathBuf,
    cloud_draft: Option<&'a Value>,
    cloud_reuse: bool,
    initial_branch: String,
    initial_reuse: bool,
    base: String,
    base_sha: String,
    base_warning: Option<String>,
    shared_deps: bool,
}

struct LockedLaneState {
    front: Front,
    body: String,
    front_id: String,
    branch: String,
    wt_dir: PathBuf,
    wt_str: String,
    wt_name: String,
    repo_label: String,
    checkout_owner: String,
    reuse: bool,
    dir_exists: bool,
    pinned_base: String,
    lane_base_sha: String,
    checked: claim::Check,
    claim_at: Option<String>,
    pre_claim: Option<(String, Option<String>, Option<model::Claim>)>,
}

struct WorktreeOutcome {
    cargo_target: Option<PathBuf>,
    created: bool,
}

struct StartOutputParams<'a> {
    front: &'a Front,
    body: &'a str,
    worktree_path: &'a Path,
    task_worktree: &'a str,
    created: bool,
    target_dir: Option<&'a PathBuf>,
    slot_env: Value,
    include_job_id_field: bool,
    record_lease_warnings: bool,
}

fn validate_pre_claim(req: &StartRequest<'_>) -> Result<PreClaimContext> {
    // Daemon reachability + spec readability are checked before any
    // filesystem or tracker mutation — a `--job` that cannot be opened
    // must leave nothing behind.
    let job_probe = match &req.args.job {
        Some(job) => Some(check_job(req.state_dir, job)?),
        None => None,
    };
    if req.args.job.is_none() {
        if let Some(owner) = &req.args.owner {
            // `issue_kickoff` may mint the worker after start; only an
            // already-registered sandbox owner is refused here.
            match client::rpc(req.state_dir, "agent_show", json!({"alias": owner})) {
                Ok(show) if show["agent"]["enabled"].as_bool() == Some(false) => {
                    return Err(Error::rejected(format!(
                        "Agent '{owner}' is disabled — resume it before starting work"
                    )));
                }
                Ok(show) if show["agent"]["endpoint_kind"].as_str() == Some("sandbox") => {
                    return Err(Error::rejected(format!(
                        "Cloud sandbox agent '{owner}' runs only `--job` dispatches — \
                         nothing was created"
                    )));
                }
                Ok(_) => {}
                Err(error)
                    if error.to_string().contains("Unknown managed agent")
                        || error.to_string().contains("Daemon is not reachable") =>
                {
                    // No job and no live daemon: the ordinary dispatch path
                    // will report the daemon failure at its own gate.
                }
                Err(error) => return Err(error),
            }
        }
    }
    let (project, dir) = write::issue_dir(req.pm, req.id)?;
    let (initial_front, initial_body) = write::load_front(&dir)?;
    let initial_intake =
        crate::issue::plan::gate_snapshot(&req.pm.dir, &initial_front, &initial_body)?;
    if let (Some(job), Some((_, spec, raw_digest))) = (req.verified_job, &job_probe) {
        let canonical = Path::new(spec)
            .canonicalize()
            .ok()
            .zip(dir.join("issue.md").canonicalize().ok())
            .is_some_and(|(spec, issue)| spec == issue);
        if !(canonical && initial_intake.is_some()) {
            let digest = crate::issue::dispatch::canonical_issue_spec_digest(
                req.pm,
                req.id,
                Path::new(spec),
                true,
            )?
            .unwrap_or_else(|| raw_digest.clone());
            if job["spec_sha256"].as_str() != Some(digest.as_str()) {
                return Err(Error::rejected(
                    "draft job spec changed; reopen or restore the pinned spec",
                ));
            }
        }
    }
    // CAD-202: a pty assignee whose pane cwd is deleted or outside the
    // project's repos refuses here, before anything is created.
    let mut cloud = false;
    let cwd_override = match &req.args.job {
        Some(JobArgs {
            assignee: Some(assignee),
            force,
            ..
        }) => {
            let show = client::rpc(req.state_dir, "agent_show", json!({"alias": assignee}))?;
            if show["agent"]["enabled"].as_bool() == Some(false) {
                return Err(Error::rejected(format!(
                    "Agent '{assignee}' is disabled — resume it before starting work"
                )));
            }
            cloud = show["agent"]["endpoint_kind"].as_str() == Some("sandbox");
            crate::issue::dispatch::check_lane_cwd(&project, assignee, &show["agent"], *force)?
        }
        _ => None,
    };
    Ok(PreClaimContext {
        project,
        dir,
        initial_front,
        initial_intake,
        job_probe,
        cloud,
        cwd_override,
    })
}

fn resolve_initial_base<'a>(
    args: &StartArgs,
    pre: &PreClaimContext,
    verified_draft: Option<(&'a Value, &'a Value)>,
) -> Result<InitialBaseContext<'a>> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = resolve_repo(&pre.project, args.repo.as_deref(), &cwd)?;
    let cloud_draft = verified_draft.filter(|_| pre.cloud).map(|(_, task)| task);
    let cloud_reuse = cloud_draft.is_some();
    let (initial_lane, initial_branch) = if pre.cloud {
        let (branch, _) = resolve_cloud_branch(&pre.initial_front, args.name.as_deref())?;
        (root.clone(), branch)
    } else {
        resolve_lane(&pre.initial_front, args.name.as_deref(), &root)?
    };
    let initial_open = open_worktrees(&pre.initial_front);
    let initial_branch_exists = !pre.cloud
        && git(
            &root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{initial_branch}"),
            ],
        )
        .is_ok();
    if !pre.cloud {
        refuse_terminal_lifecycle_record(
            &root,
            &initial_lane,
            &pre.initial_front,
            &initial_branch,
            initial_branch_exists,
        )?;
    }
    let initial_lane_str = initial_lane.to_string_lossy().into_owned();
    let initial_refs_recorded =
        pre.initial_front
            .refs
            .iter()
            .any(|r| r.kind == "branch" && r.path.as_deref() == Some(initial_branch.as_str()))
            && pre.initial_front.refs.iter().any(|r| {
                r.kind == "worktree" && r.path.as_deref() == Some(initial_lane_str.as_str())
            });
    let initial_lifecycle_record = if pre.cloud {
        None
    } else {
        worktree::lifecycle::recoverable_record(
            &root,
            &initial_lane,
            "development",
            "cadence issue start",
            Some(&initial_branch),
            Some(&pre.initial_front.id),
        )?
    };
    let initial_reuse = initial_branch_exists
        && (!initial_open.is_empty()
            || initial_refs_recorded
            || initial_lifecycle_record.is_some());
    let verified_job = verified_draft.map(|(job, _)| job);
    let (base, base_sha, base_warning) = if cloud_reuse {
        let base = verified_job
            .and_then(|job| job["base_ref"].as_str())
            .ok_or_else(|| Error::rejected("verified cloud draft has no base"))?;
        (base.to_string(), base.to_string(), None)
    } else if initial_reuse && args.base.is_none() {
        let (name, sha) = resolve_existing_base(&root, &initial_branch)?;
        (name, sha, None)
    } else {
        let resolved = resolve_base(
            &root,
            args.base.as_deref(),
            configured_integration(&pre.project, &root)?,
        )?;
        (resolved.name, resolved.sha, resolved.warning)
    };
    let shared_deps = worktree::shared_deps_enabled(&pre.project)?;
    Ok(InitialBaseContext {
        root,
        cloud_draft,
        cloud_reuse,
        initial_branch,
        initial_reuse,
        base,
        base_sha,
        base_warning,
        shared_deps,
    })
}

fn acquire_and_record_claim(
    req: &StartRequest<'_>,
    actors: &StartActors,
    pre: &PreClaimContext,
    init: &InitialBaseContext<'_>,
) -> Result<LockedLaneState> {
    // CAD-1191: lock 1 (short) — claim check, take-over and, for a new
    // lane, the claim record. Worktree work runs unlocked; lock 2
    // records the lane refs.
    if pre.initial_intake.is_some() {
        crate::issue::lockseam::outside_lock("start-pre-claim");
    }
    let lock = req.pm.lock()?;
    let (mut front, body) = write::load_front(&pre.dir)?;
    let front_id = front.id.clone();
    // CAD-383: a doing/review issue held by someone else refuses before
    // any lane is touched; a take-over is its own commit, made now.
    let mut asking = vec![actors.requester.as_str()];
    asking.extend(actors.new_owner.as_deref());
    let checked = claim::check(
        &front,
        &asking,
        req.args.take_over.as_deref(),
        "issue start",
        || {
            claim::since(
                &req.pm.dir,
                &pre.project.key,
                &front,
                Duration::from_secs(2),
            )
        },
    )?;
    // CAD-274: an open worktree ref IS the issue's lane — a re-start
    // reuses it (re-applying the cargo target) rather than minting a
    // second lane from the title.
    let open = open_worktrees(&front);
    let (wt_dir, branch, cloud_branch_recorded) = if pre.cloud {
        let (branch, recorded) = resolve_cloud_branch(&front, req.args.name.as_deref())?;
        if branch != init.initial_branch
            || init
                .cloud_draft
                .is_some_and(|task| !recorded || task["branch"].as_str() != Some(branch.as_str()))
        {
            return Err(Error::rejected("verified cloud job branch binding changed"));
        }
        (init.root.clone(), branch, recorded)
    } else {
        let (lane, branch) = resolve_lane(&front, req.args.name.as_deref(), &init.root)?;
        (lane, branch, false)
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
    let (dir_exists, branch_exists, lifecycle_record) = if pre.cloud {
        (false, false, None)
    } else {
        let branch_exists = git(
            &init.root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
        .is_ok();
        refuse_terminal_lifecycle_record(&init.root, &wt_dir, &front, &branch, branch_exists)?;
        let record = worktree::lifecycle::recoverable_record(
            &init.root,
            &wt_dir,
            "development",
            "cadence issue start",
            Some(&branch),
            Some(&front.id),
        )?;
        (wt_dir.is_dir(), branch_exists, record)
    };
    let reuse = init.cloud_reuse
        || branch_exists
            && (!open.is_empty()
                || recorded("branch", &branch) && recorded("worktree", &wt_str)
                || lifecycle_record.is_some());
    if init.initial_reuse && !reuse && req.args.base.is_none() {
        return Err(Error::rejected(
            "the recorded lane changed while issue start was waiting; retry to refresh and pin the default base",
        ));
    }
    let pinned_base = if init.cloud_reuse {
        init.base_sha.clone()
    } else if reuse && pre.initial_intake.is_some() {
        match lifecycle_record.as_ref().and_then(|r| r.base_sha.as_ref()) {
            Some(recorded) => recorded.clone(),
            None => intake_branch_creation_base(&init.root, &branch)?,
        }
    } else if branch_exists {
        branch_base(&init.root, &branch, &init.base_sha)
    } else {
        init.base_sha.clone()
    };
    let lane_base_sha = if reuse {
        pinned_base.clone()
    } else {
        init.base_sha.clone()
    };
    // Validate the actual selected lane under the same tracker lock as
    // claim, including a reused checkout that ignores an explicit --base.
    crate::issue::plan::gate_claim(&req.pm.dir, &front, &body, &lane_base_sha)?;
    // A cloud start's admission is a read-only AOS probe, before the
    // take-over commit and before the ordinary new-lane claim. A denied
    // company leaves the tracker untouched.
    if pre.cloud {
        client::rpc(req.state_dir, "cloud_probe", json!({}))?;
    }
    if let Some(t) = &checked.take_over {
        let mut taken = front.clone();
        taken.claim = Some(claim::new_claim(&actors.requester, Some(t.reason.clone())));
        taken.owner = Some(
            actors
                .new_owner
                .clone()
                .unwrap_or_else(|| actors.requester.clone()),
        );
        let (text, subject) = claim::take_over_record(&actors.requester, t);
        write::commit_front_with_comment(
            req.pm,
            &pre.dir,
            &front,
            &taken,
            &body,
            &actors.requester,
            "claim",
            &text,
            &subject,
            req.actor,
        )?;
        front = taken;
    }
    let repo_label = init
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| init.root.display().to_string());
    let checkout_owner = front
        .owner
        .clone()
        .or_else(|| actors.new_owner.clone())
        .unwrap_or_else(|| actors.requester.clone());
    let first_recorded = |kind: &str| {
        front
            .refs
            .iter()
            .find(|r| r.kind == kind)
            .and_then(|r| r.path.clone())
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
                actors
                    .new_owner
                    .clone()
                    .unwrap_or_else(|| write::actor_who(req.actor, req.args.by.as_deref())),
            );
        }
        // CAD-383: the start that puts the issue into work claims it —
        // the requester (the dispatching PM), not the lane.
        if claimed.claim.is_none() || checked.warning.is_some() {
            claimed.claim = Some(claim::new_claim(&actors.requester, None));
        }
        claim_at = claimed.claim.as_ref().map(|c| c.at.clone());
        let record_cloud_branch = pre.cloud && !cloud_branch_recorded;
        if record_cloud_branch {
            claimed.refs.push(write::new_ref(
                "branch",
                &branch,
                Some(crate::issue::cloud::CLOUD_BRANCH_LABEL),
                None,
                None,
            ));
        }
        if claimed.status != front.status
            || claimed.owner != front.owner
            || claimed.claim != front.claim
            || record_cloud_branch
        {
            let committed = write::save_front(&pre.dir, &claimed, &body).and_then(|_| {
                write::commit_who(
                    req.pm,
                    &[pre.dir.join("issue.md")],
                    &format!("{}: start {branch} (claim)", front.id),
                    &[front.id.as_str()],
                    req.actor,
                    Some(&actors.requester),
                )
            });
            if let Err(e) = committed {
                let _ = write::save_front(&pre.dir, &front, &body);
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
    Ok(LockedLaneState {
        front,
        body,
        front_id,
        branch,
        wt_dir,
        wt_str,
        wt_name,
        repo_label,
        checkout_owner,
        reuse,
        dir_exists,
        pinned_base,
        lane_base_sha,
        checked,
        claim_at,
        pre_claim,
    })
}

fn build_checkout_record(root: &Path, state: &LockedLaneState) -> worktree::lifecycle::Checkout {
    let mut record = worktree::lifecycle::new_record(worktree::lifecycle::CheckoutSpec {
        repo: root,
        purpose: "development",
        tool: "cadence issue start",
        owner: &state.checkout_owner,
        path: &state.wt_dir,
        branch: Some(&state.branch),
        pinned_sha: &state.pinned_base,
        issue: Some(&state.front_id),
    });
    record.base_sha = Some(state.pinned_base.clone());
    record
}

fn fail_setup(root: &Path, wt_dir: &Path, err: Error) -> Error {
    let _ = worktree::lifecycle::transition(root, wt_dir, "setup-failed", Some(&err.to_string()));
    err
}

fn setup_worktree_lane(
    init: &InitialBaseContext<'_>,
    state: &LockedLaneState,
) -> Result<WorktreeOutcome> {
    let root = &init.root;
    // Unlocked: worktree creation and setup (cargo target, hooks). The
    // lifecycle ledger records the intent first, so a crash here leaves
    // an inventoried `preparing` checkout that a re-run reuses.
    crate::issue::lockseam::outside_lock("start-worktree");
    // Clear registrations whose dirs are gone — a deleted worktree
    // must not block its own re-creation.
    let _ = git(root, &["worktree", "prune"]);
    let mut created = false;
    let cargo_target: Option<PathBuf>;
    if state.reuse {
        let reattached = !state.dir_exists;
        if reattached {
            // Refs still accurate — record the interrupted recovery before
            // re-attaching the surviving branch.
            worktree::ensure_cadence_ignored(root)?;
            worktree::lifecycle::begin(root, build_checkout_record(root, state))?;
            worktree::add(root, &state.wt_dir, None, &state.branch)
                .map_err(|e| fail_setup(root, &state.wt_dir, e))?;
        } else {
            worktree::validate_registered_branch(root, &state.wt_dir, &state.branch)?;
        }
        cargo_target = worktree::setup_development(&state.wt_dir, root, init.shared_deps)
            .map_err(|e| fail_setup(root, &state.wt_dir, e))?;
        // Reused lanes re-heal shared cargo setup and the per-worktree hook.
        worktree::lifecycle::activate(root, build_checkout_record(root, state))
            .map_err(|e| fail_setup(root, &state.wt_dir, e))?;
    } else {
        // Persist intent first: if the process stops during setup, inventory
        // reports an interrupted checkout instead of inferring ownership from
        // its directory name.
        worktree::ensure_cadence_ignored(root)?;
        worktree::lifecycle::begin(root, build_checkout_record(root, state))?;
        worktree::add(root, &state.wt_dir, Some(&state.branch), &init.base_sha)
            .map_err(|e| fail_setup(root, &state.wt_dir, e))?;
        // Existing build-target and pre-push setup must succeed before the
        // checkout becomes active in the tracker.
        cargo_target = worktree::setup_development(&state.wt_dir, root, init.shared_deps)
            .map_err(|e| fail_setup(root, &state.wt_dir, e))?;
        worktree::lifecycle::activate(root, build_checkout_record(root, state))
            .map_err(|e| fail_setup(root, &state.wt_dir, e))?;
        created = true;
    }
    Ok(WorktreeOutcome {
        cargo_target,
        created,
    })
}

fn rollback_failed_claim(
    req: &StartRequest<'_>,
    actors: &StartActors,
    dir: &Path,
    state: &LockedLaneState,
) -> Result<()> {
    // A refused new lane leaves the issue as it was: give the
    // claim back (only if it is still the one lock 1 recorded).
    if let (Some((status, owner, prev_claim)), Some(at)) = (&state.pre_claim, &state.claim_at) {
        let _lock = req.pm.lock()?;
        let (mut cur, cur_body) = write::load_front(dir)?;
        if cur
            .claim
            .as_ref()
            .is_some_and(|c| c.by == actors.requester && &c.at == at)
        {
            cur.status = status.clone();
            cur.owner = owner.clone();
            cur.claim = prev_claim.clone();
            let undone = write::save_front(dir, &cur, &cur_body).and_then(|_| {
                write::commit_who(
                    req.pm,
                    &[dir.join("issue.md")],
                    &format!(
                        "{}: start {} (claim released: setup failed)",
                        cur.id, state.branch
                    ),
                    &[cur.id.as_str()],
                    req.actor,
                    Some(&actors.requester),
                )
            });
            if undone.is_err() {
                let _ = write::save_front(dir, &state.front, &cur_body);
            }
        }
    }
    Ok(())
}

fn verify_and_record_lane_refs(
    req: &StartRequest<'_>,
    actors: &StartActors,
    pre: &PreClaimContext,
    init: &InitialBaseContext<'_>,
    state: &LockedLaneState,
    outcome: &WorktreeOutcome,
) -> Result<(Front, String)> {
    // Lock 2 (short): re-load the issue and prove the claim is still
    // ours before recording the lane. The refs are merged onto the fresh
    // front, so an unrelated edit made meanwhile (a comment, a priority
    // change) is kept, never overwritten and never a reason to refuse.
    let lock = req.pm.lock()?;
    let (cur, cur_body) = write::load_front(&pre.dir)?;
    let claim_lost = |why: String| {
        Error::rejected(format!(
            "{}: {why} while the lane was being set up — refs for {} were not \
             recorded. The worktree {} and its lifecycle record are left in place; \
             re-run `cadence issue start {}` to reuse the lane once the claim is yours",
            cur.id,
            state.branch,
            state.wt_dir.display(),
            cur.id
        ))
    };
    if outcome.created || state.claim_at.is_some() {
        // A new lane: the claim lock 1 recorded must still be ours.
        let ours = cur
            .claim
            .as_ref()
            .is_some_and(|c| c.by == actors.requester && Some(&c.at) == state.claim_at.as_ref());
        if !ours {
            return Err(claim_lost(format!(
                "the claim on it changed (now held by {})",
                claim::holders(&cur).join(", ")
            )));
        }
    } else {
        // A reused lane: the same claim check lock 1 ran, with no
        // take-over available — only a holder that is not us refuses.
        let mut asking = vec![actors.requester.as_str()];
        asking.extend(actors.new_owner.as_deref());
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
    let refreshed = with_lane_refs(
        &cur,
        &state.branch,
        &state.wt_str,
        &state.repo_label,
        &outcome.cargo_target,
    );
    let mut front = cur;
    if refreshed.refs != front.refs {
        // `Actor:` is the requester (the dispatching PM), so the lane's
        // advisory code-area PM is bound to this record and not to live
        // frontmatter an agent can rewrite.
        let subject = if outcome.created {
            format!("{}: start {}", front.id, state.branch)
        } else {
            format!("{}: start {} (refs refreshed)", front.id, state.branch)
        };
        let committed = write::save_front(&pre.dir, &refreshed, &body).and_then(|_| {
            write::commit_who(
                req.pm,
                &[pre.dir.join("issue.md")],
                &subject,
                &[front.id.as_str()],
                req.actor,
                Some(&actors.requester),
            )
        });
        if let Err(e) = committed {
            let _ = write::save_front(&pre.dir, &front, &body);
            if outcome.created {
                // The managed checkout stays in setup-failed state for an
                // explicit retry.
                return Err(fail_setup(&init.root, &state.wt_dir, e));
            }
            return Err(e);
        }
        front = refreshed;
    }
    drop(lock);
    Ok((front, body))
}

fn build_start_output(
    init: &InitialBaseContext<'_>,
    state: &LockedLaneState,
    cloud: bool,
    params: &StartOutputParams<'_>,
) -> Value {
    let mut claim_out = claim::json(params.front, crate::issue::time::now_epoch());
    claim_out["warning"] = json!(state.checked.warning);
    claim_out["take_over"] = json!(state
        .checked
        .take_over
        .as_ref()
        .map(|t| json!({"from": t.from, "reason": t.reason})));
    let mut out = json!({
        "issue": params.front.id,
        "repo": init.root,
        "worktree": params.worktree_path,
        "branch": state.branch,
        "base": {"ref": init.base, "sha": state.lane_base_sha},
        "trailer": format!("Issue: {}", params.front.id),
        "created": params.created,
        "target_dir": params.target_dir,
        "slot_env": params.slot_env,
        "claim": claim_out,
    });
    if cloud {
        out["cloud"] = json!(true);
    }
    if let Some(warning) = &init.base_warning {
        eprintln!("cadence: {warning}");
        out["base"]["warning"] = json!(warning);
    }
    out
}

fn build_and_finalize_start(
    req: &StartRequest<'_>,
    actors: &StartActors,
    pre: &PreClaimContext,
    init: &InitialBaseContext<'_>,
    state: &LockedLaneState,
    params: &StartOutputParams<'_>,
) -> Result<Value> {
    let mut out = build_start_output(init, state, pre.cloud, params);
    if let (Some(job), Some((job_state_dir, spec, spec_sha256))) = (&req.args.job, &pre.job_probe) {
        let task_acceptance = if pre.initial_intake.is_some() {
            None
        } else {
            crate::task_acceptance::acceptance_listing(&parse::acceptance_items(params.body))
        };
        let created_job = if let Some(job) = req.verified_job {
            json!({"job": {"id": job["id"]}})
        } else {
            let mut payload = json!({
                "pm": job.pm,
                "spec": spec,
                "spec_sha256": spec_sha256,
                "title": params.front.title,
                "issue": params.front.id,
                "repo": init.root,
                "base_ref": state.lane_base_sha,
                "task_worktree": params.task_worktree,
                "task_branch": state.branch,
                "task_base_sha": state.lane_base_sha,
                "task_assignee": job.assignee,
                "task_acceptance": task_acceptance,
            });
            if params.include_job_id_field {
                payload["job"] = json!(job.id);
            }
            client::rpc(job_state_dir, "job_new", payload)?
        };
        let job_id = created_job["job"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        out["job"] = json!(job_id.clone());
        // `job_new` mints exactly one task — `<job>-t1` — scoped above.
        out["task"] = json!(format!("{job_id}-t1"));
    }
    if let Some(note) = &pre.cwd_override {
        // The override is part of the issue's record, not just output.
        write::add_comment(
            req.pm,
            req.id,
            note,
            None,
            Some("dispatch"),
            None,
            req.actor,
        )?;
        out["cwd_override"] = json!(note);
    }
    // CAD-378: advisory path leases — the ticket's planned paths
    // against the project's code areas and the other open lanes. Never
    // refuses: the warnings ride the output, and a start that minted
    // the lane records them on the issue as a `lease` comment (the
    // event). A failure to record is reported, not raised.
    let mut leases = crate::issue::areas::check_start(
        &req.pm.dir,
        req.state_dir,
        &pre.project.key,
        params.front,
        &actors.requester,
    );
    let lines = crate::issue::areas::warning_lines(&leases);
    if params.record_lease_warnings && !lines.is_empty() {
        let text = format!("Lease warnings at start:\n- {}", lines.join("\n- "));
        if let Err(e) =
            write::add_comment(req.pm, req.id, &text, None, Some("lease"), None, req.actor)
        {
            leases["record_error"] = json!(e.to_string());
        }
    }
    out["leases"] = leases;
    Ok(out)
}

pub(super) fn run_with_verified_job(
    pm: &Pm,
    id: &str,
    args: &StartArgs,
    actor: &str,
    state_dir: &Path,
    verified_draft: Option<(&Value, &Value)>,
) -> Result<Value> {
    let req = StartRequest {
        pm,
        id,
        args,
        actor,
        state_dir,
        verified_job: verified_draft.map(|(job, _)| job),
    };
    let pre = validate_pre_claim(&req)?;
    let init = resolve_initial_base(args, &pre, verified_draft)?;

    // CAD-383: who is asking, and the owner this start would record.
    if let Some(by) = &args.by {
        claim::check_alias(by, "--by")?;
    }
    let actors = StartActors {
        requester: write::actor_who(
            actor,
            args.by
                .as_deref()
                .or(args.job.as_ref().map(|j| j.pm.as_str())),
        ),
        new_owner: args
            .owner
            .clone()
            .or_else(|| args.job.as_ref().and_then(|j| j.assignee.clone())),
    };

    let locked = acquire_and_record_claim(&req, &actors, &pre, &init)?;

    if pre.cloud {
        // A cloud agent owns no host worktree. The remote runner clones the
        // project repo. Record only the branch lane; the job carries the
        // host repo path and branch/base for the daemon-side runner.
        let root_str = init.root.to_string_lossy();
        let params = StartOutputParams {
            front: &locked.front,
            body: &locked.body,
            worktree_path: &init.root,
            task_worktree: &root_str,
            created: true,
            target_dir: None,
            slot_env: Value::Null,
            include_job_id_field: false,
            record_lease_warnings: true,
        };
        return build_and_finalize_start(&req, &actors, &pre, &init, &locked, &params);
    }

    let outcome = match setup_worktree_lane(&init, &locked) {
        Ok(done) => done,
        Err(e) => {
            rollback_failed_claim(&req, &actors, &pre.dir, &locked)?;
            return Err(e);
        }
    };

    let (front, body) = verify_and_record_lane_refs(&req, &actors, &pre, &init, &locked, &outcome)?;

    // CAD-113: the worktree's slot environment — a write failure is
    // reported, never silently swallowed.
    let slot_env = match write_slot_env(&locked.wt_dir, &pm.dir) {
        Ok(path) => json!({"path": path}),
        Err(e) => {
            let _ = worktree::lifecycle::transition(
                &init.root,
                &locked.wt_dir,
                "setup-failed",
                Some(&e.to_string()),
            );
            json!({"error": e.to_string(), "recovery": format!("retry issue start to reapply setup: {e}")})
        }
    };
    let params = StartOutputParams {
        front: &front,
        body: &body,
        worktree_path: &locked.wt_dir,
        task_worktree: &locked.wt_name,
        created: outcome.created,
        target_dir: outcome.cargo_target.as_ref(),
        slot_env,
        include_job_id_field: true,
        record_lease_warnings: outcome.created,
    };
    build_and_finalize_start(&req, &actors, &pre, &init, &locked, &params)
}

/// CAD-113: the worktree's build-slot environment — `CARGO_BUILD_JOBS`
/// from `[host] jobs_per_lane` (default 2) and the `build-slot` helper
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
        .unwrap_or(2);
    let helper = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cadence"));
    let file = wt_dir.join(".env");
    let owned = [
        "CARGO_BUILD_JOBS=",
        "CADENCE_BUILD_SLOT=",
        "export CADENCE_BUILD_SLOT=",
    ];
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
    text.push_str(&format!(
        "export CADENCE_BUILD_SLOT='{}'\n",
        helper.display().to_string().replace('\'', "'\\''")
    ));
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
    crate::worktree::record_lane_env(wt_dir, &text);
    Ok(file)
}

#[cfg(test)]
mod resolve_base_tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::resolve_base;

    fn run_git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn rev(dir: &Path, spec: &str) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", spec])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        checkout: PathBuf,
        main_sha: String,
        staging_sha: String,
        side_sha: String,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let checkout = tmp.path().join("checkout");
        std::fs::create_dir(&origin).unwrap();
        run_git(
            &origin,
            &["init", "--quiet", "--bare", "--initial-branch=main"],
        );
        std::fs::create_dir(&checkout).unwrap();
        run_git(&checkout, &["init", "--quiet", "--initial-branch=main"]);
        run_git(&checkout, &["config", "user.email", "t@example.com"]);
        run_git(&checkout, &["config", "user.name", "t"]);
        std::fs::write(checkout.join("f"), "main\n").unwrap();
        run_git(&checkout, &["add", "f"]);
        run_git(&checkout, &["commit", "--quiet", "-m", "main"]);
        let main_sha = rev(&checkout, "HEAD");
        run_git(
            &checkout,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        run_git(
            &checkout,
            &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
        );
        run_git(&checkout, &["checkout", "--quiet", "-b", "staging"]);
        std::fs::write(checkout.join("s"), "staging\n").unwrap();
        run_git(&checkout, &["add", "s"]);
        run_git(&checkout, &["commit", "--quiet", "-m", "staging"]);
        let staging_sha = rev(&checkout, "HEAD");
        run_git(
            &checkout,
            &["push", "--quiet", "origin", "HEAD:refs/heads/staging"],
        );
        run_git(&checkout, &["checkout", "--quiet", "-b", "side", &main_sha]);
        std::fs::write(checkout.join("x"), "foreign\n").unwrap();
        run_git(&checkout, &["add", "x"]);
        run_git(&checkout, &["commit", "--quiet", "-m", "foreign"]);
        let side_sha = rev(&checkout, "HEAD");
        run_git(&checkout, &["fetch", "--quiet", "origin"]);
        // Reproduce a checkout whose clone never recorded origin/HEAD.
        // `--no-deref` matters: without it `update-ref -d` resolves the symbolic
        // ref and deletes its *target*, leaving the symref in place — which is
        // how this fixture passed locally (no symref to begin with) and failed
        // in CI (a fresh clone writes one).
        let _ = Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["update-ref", "--no-deref", "-d", "refs/remotes/origin/HEAD"])
            .output();
        let head = Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
            .output()
            .unwrap();
        assert!(!head.status.success(), "fixture must not have origin/HEAD");
        Fixture {
            _tmp: tmp,
            checkout,
            main_sha,
            staging_sha,
            side_sha,
        }
    }

    #[test]
    fn parked_checkout_does_not_become_the_lane_base() {
        let fix = fixture();
        let resolved = resolve_base(&fix.checkout, None, None).expect("remote HEAD");
        assert_eq!(resolved.name, "origin/main");
        assert_eq!(resolved.sha, fix.main_sha);
        assert_ne!(resolved.sha, fix.side_sha);
        let warning = resolved.warning.expect("foreign checkout is named");
        assert!(warning.contains("side"), "{warning}");
        assert!(warning.contains("origin/main"), "{warning}");
        assert!(!warning.contains(&fix.side_sha), "{warning}");
    }

    #[test]
    fn configured_base_is_the_integration_ref() {
        let fix = fixture();
        let resolved = resolve_base(&fix.checkout, None, Some("staging")).expect("configured");
        assert_eq!(resolved.name, "origin/staging");
        assert_eq!(resolved.sha, fix.staging_sha);
        assert_ne!(resolved.sha, fix.side_sha);
        assert!(resolved.warning.is_some());
    }

    #[test]
    fn explicit_base_is_the_operator_override() {
        let fix = fixture();
        let resolved = resolve_base(&fix.checkout, Some("side"), None).expect("override");
        assert_eq!(resolved.sha, fix.side_sha);
        assert!(resolved.warning.is_none());
    }

    #[test]
    fn explicit_branch_base_is_the_remote_tip_not_a_stale_local_ref() {
        let fix = fixture();
        // Advance origin/main from a second clone; the checkout's local
        // `main` and its remote-tracking ref both stay behind.
        let other = fix._tmp.path().join("other");
        let origin = fix._tmp.path().join("origin.git");
        run_git(
            fix._tmp.path(),
            &[
                "clone",
                "--quiet",
                origin.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        run_git(&other, &["config", "user.email", "t@example.com"]);
        run_git(&other, &["config", "user.name", "t"]);
        std::fs::write(other.join("n"), "newer\n").unwrap();
        run_git(&other, &["add", "n"]);
        run_git(&other, &["commit", "--quiet", "-m", "newer"]);
        run_git(
            &other,
            &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
        );
        let remote_tip = rev(&other, "HEAD");
        assert_ne!(remote_tip, fix.main_sha);
        assert_eq!(rev(&fix.checkout, "main"), fix.main_sha);

        let resolved = resolve_base(&fix.checkout, Some("main"), None).expect("fresh");
        assert_eq!(resolved.name, "main");
        assert_eq!(resolved.sha, remote_tip);

        // A full SHA is used as given.
        let pinned = resolve_base(&fix.checkout, Some(&fix.main_sha), None).expect("sha");
        assert_eq!(pinned.sha, fix.main_sha);
    }

    #[test]
    fn explicit_base_ignores_a_local_ref_shadowing_origin_main() {
        let fix = fixture();
        // A local tag named `origin/main` outranks the short remote-tracking name.
        run_git(&fix.checkout, &["tag", "origin/main", &fix.side_sha]);
        let resolved = resolve_base(&fix.checkout, Some("main"), None).expect("main");
        assert_eq!(resolved.sha, fix.main_sha);
        let resolved = resolve_base(&fix.checkout, Some("origin/main"), None).expect("origin/main");
        assert_eq!(resolved.sha, fix.main_sha);
    }

    #[test]
    fn explicit_remote_base_refuses_a_branch_origin_dropped() {
        let fix = fixture();
        run_git(
            &fix.checkout,
            &["push", "--quiet", "origin", "--delete", "staging"],
        );
        // The push pruned the tracking ref; restore it as a stale leftover.
        run_git(
            &fix.checkout,
            &[
                "update-ref",
                "refs/remotes/origin/staging",
                &fix.staging_sha,
            ],
        );
        for base in ["origin/staging", "refs/remotes/origin/staging"] {
            let error = resolve_base(&fix.checkout, Some(base), None).expect_err(base);
            assert!(error.to_string().contains("no longer carries"), "{error}");
        }
    }

    #[test]
    fn explicit_origin_head_resolves_the_remote_head_branch() {
        let fix = fixture();
        let resolved = resolve_base(&fix.checkout, Some("origin/HEAD"), None).expect("head");
        assert_eq!(resolved.sha, fix.main_sha);
    }

    #[test]
    fn explicit_branch_base_refuses_when_origin_is_unreachable() {
        let fix = fixture();
        run_git(
            &fix.checkout,
            &["remote", "set-url", "origin", "/nonexistent/origin.git"],
        );
        let error = resolve_base(&fix.checkout, Some("main"), None).expect_err("must refuse");
        assert!(
            error.to_string().contains("Could not reach origin"),
            "{error}"
        );
        // A full SHA does not need the remote.
        resolve_base(&fix.checkout, Some(&fix.main_sha), None).expect("sha");
    }

    #[test]
    fn a_repo_with_no_origin_still_uses_its_current_branch() {
        let fix = fixture();
        run_git(&fix.checkout, &["remote", "remove", "origin"]);
        let resolved = resolve_base(&fix.checkout, None, None).expect("local only");
        assert_eq!(resolved.name, "side");
        assert_eq!(resolved.sha, fix.side_sha);
        assert!(resolved.warning.is_none());
    }
}
