//! Job task spec resolution, scope proof, bounded reading, and contract assembly.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::{
    content_digest, context_guard, parse_acceptance_listing, GatedIssue, JobContext,
    PreparedContext,
};
use crate::error::{Error, Result};
use crate::issue::app_catalog::fs::Root;
use crate::issue::parse::AcceptanceItem;
use crate::issue::{context, parse, start, write, Pm};
use crate::store;

/// Does `lane`'s directory name belong to issue `id` under the layout
/// (`<id-lower>-<slug>`)? A layout lane named for another issue is a foreign
/// source even when it lives under the same declared repository.
fn lane_of_issue(lane: &Path, issue_id: &str) -> bool {
    lane.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.starts_with(&crate::worktree::layout::issue_stem(issue_id)))
}

/// The canonical task lane a recorded `worktree` names: an exact layout lane
/// (`<declared repo>/.cadence/wt/<name>`) of this issue, below a repository
/// the project declares. A bare name is resolved under the declared repos; an
/// ambiguous match is refused rather than guessed, and a path of any other
/// shape is never an approved lane.
fn task_spec_lane(declared: &[PathBuf], worktree: Option<&str>, issue_id: &str) -> Option<PathBuf> {
    let worktree = worktree?;
    let raw = Path::new(worktree);
    if raw.is_absolute() {
        let lane = raw.canonicalize().ok()?;
        let root = crate::worktree::layout::root_of(&lane)?;
        return (declared.contains(&root)
            && lane.starts_with(crate::worktree::layout::worktrees_dir(&root))
            && lane_of_issue(&lane, issue_id))
        .then_some(lane);
    }
    let mut parts = raw.components();
    match (parts.next(), parts.next()) {
        (Some(std::path::Component::Normal(_)), None) => {}
        _ => return None,
    }
    let mut found = declared.iter().filter_map(|root| {
        let lane = crate::worktree::layout::worktree_dir(root, worktree)
            .canonicalize()
            .ok()?;
        (crate::worktree::layout::root_of(&lane).as_ref() == Some(root)
            && lane_of_issue(&lane, issue_id))
        .then_some(lane)
    });
    let lane = found.next()?;
    if found.next().is_some() {
        return None;
    }
    Some(lane)
}

/// The base a relative *task override* resolves against: the task's own lane,
/// or the declared repo root a cloud task records as its worktree. Never the
/// daemon's working directory.
fn task_spec_base(declared: &[PathBuf], worktree: Option<&str>, issue_id: &str) -> Option<PathBuf> {
    if let Some(lane) = task_spec_lane(declared, worktree, issue_id) {
        return Some(lane);
    }
    let raw = Path::new(worktree?);
    if !raw.is_absolute() {
        return None;
    }
    let base = raw.canonicalize().ok()?;
    (base.is_dir() && declared.contains(&base)).then_some(base)
}

/// The base a relative *job* spec resolves against: the repository the job
/// declares, else the project's single declared repository. Zero or several
/// candidates is ambiguity, refused rather than guessed.
fn default_spec_base(declared: &[PathBuf], job_repo: Option<&str>) -> Option<PathBuf> {
    match job_repo {
        Some(repo) => {
            let repo = crate::issue::project::expand_home(repo);
            let repo = repo.canonicalize().ok()?;
            declared.iter().find(|root| **root == repo).cloned()
        }
        None => match declared {
            [only] => Some(only.to_path_buf()),
            _ => None,
        },
    }
}

/// Is `rel` a tracked regular blob at `root`'s HEAD? The bounded `ls-tree`
/// proof the project-context reader uses, without its document allowlist: a
/// task spec may be any tracked text file. This tests the server-derived
/// repository, never the caller's path as a capability, and makes no claim
/// that the working bytes match HEAD.
fn tracked_at_head(root: &Path, rel: &Path) -> Result<bool> {
    let path = rel.to_str().ok_or_else(|| {
        Error::rejected("context refused: spec-scope-unverifiable: the task spec path is not UTF-8")
    })?;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["--literal-pathspecs", "ls-tree", "-z", "HEAD", "--", path]);
    let out = crate::proc::run_bounded(&mut cmd, Duration::from_secs(30)).map_err(|error| {
        Error::rejected(format!(
            "context refused: spec-scope-unverifiable: {} could not prove tracked task \
             specs ({error}) — nothing was read",
            root.display()
        ))
    })?;
    if !out.status.success() {
        return Ok(false);
    }
    Ok(out
        .stdout
        .split(|byte| *byte == 0)
        .next()
        .is_some_and(|record| {
            record.starts_with(b"100644 blob ") || record.starts_with(b"100755 blob ")
        }))
}

fn spec_scope_error(path: &str, why: &str) -> Error {
    crate::context_pack::Refusal::ScopeConflict {
        detail: format!("the task spec {path} is outside the approved source scope: {why}"),
    }
    .error()
}

/// Approve and read one job task spec. The canonical target must be the
/// task's own lane, the captured issue directory, or a tracked file of a
/// declared project repository; anything else is a scope conflict before a
/// byte is read. The read itself is descriptor-relative beneath the approved
/// root, nofollow, bounded to the existing local combined cap and UTF-8 only;
/// a failure states that bounded reader's actual error rather than pretending
/// the file was unavailable or verified at a commit.
fn read_job_spec(
    project: &crate::issue::project::Project,
    issue_dir: &Path,
    task: &store::Task,
    job: &store::Job,
    spec_path: &str,
) -> Result<(String, PathBuf, &'static str)> {
    let declared = start::declared_repos(project);
    let issue_id = job.issue_id.as_deref().unwrap_or_default();
    let raw = Path::new(spec_path);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        let base = if task.spec_path.is_some() {
            task_spec_base(&declared, task.worktree.as_deref(), issue_id)
        } else {
            default_spec_base(&declared, job.repo.as_deref())
        };
        let Some(base) = base else {
            return Err(spec_scope_error(
                spec_path,
                "a relative spec has no approved task lane or declared repository",
            ));
        };
        base.join(raw)
    };
    let file = candidate.canonicalize().map_err(|error| {
        Error::rejected(format!(
            "context refused: spec-unavailable: the task spec {spec_path} could not be \
             resolved ({error}) — nothing was read or queued"
        ))
    })?;
    if !file.is_file() {
        return Err(spec_scope_error(spec_path, "it is not a regular file"));
    }
    let issue_dir = issue_dir.canonicalize().map_err(|error| {
        Error::rejected(format!(
            "context refused: spec-scope-unverifiable: the captured issue directory {} \
             could not be resolved ({error}) — nothing was read",
            issue_dir.display()
        ))
    })?;
    let lane = task_spec_lane(&declared, task.worktree.as_deref(), issue_id);
    let (root, label) = if file.starts_with(&issue_dir) {
        (issue_dir, "issue")
    } else if let Some(lane) = lane.filter(|lane| file.starts_with(lane)) {
        (lane, "lane")
    } else {
        let mut approved = None;
        for root in &declared {
            if !file.starts_with(root) {
                continue;
            }
            let Ok(rel) = file.strip_prefix(root) else {
                continue;
            };
            if tracked_at_head(root, rel)? {
                approved = Some((root.to_path_buf(), "tracked-repo"));
                break;
            }
        }
        approved.ok_or_else(|| {
            spec_scope_error(
                spec_path,
                "it is outside the task lane and captured issue directory and is not a \
                 tracked file of a declared project repository",
            )
        })?
    };
    let relative = file
        .strip_prefix(&root)
        .map_err(|_| spec_scope_error(spec_path, "it is not beneath the approved root"))?;
    let reader = Root::open(&root).map_err(|error| {
        Error::rejected(format!(
            "context refused: spec-unavailable: the approved root {} could not be opened \
             by the bounded source reader ({error}) — nothing was read or queued",
            root.display()
        ))
    })?;
    let text = reader
        .read(relative, crate::context_pack::MAX_COMBINED_BYTES as u64)
        .map_err(|error| {
            Error::rejected(format!(
                "context refused: spec-unavailable: the approved task spec {} could not \
                 be read by the bounded source reader ({error}) — nothing was truncated \
                 or queued",
                file.display()
            ))
        })?;
    let Some(text) = text else {
        return Err(Error::rejected(format!(
            "context refused: spec-unavailable: the approved task spec {} does not exist",
            file.display()
        )));
    };
    Ok((text, file, label))
}

/// Canonical issue specs contain mutable tracker metadata that is not part
/// of their effective spec. Pin every body byte, including all criteria;
/// retain the separate raw source digest in the delivery manifest.
pub fn canonical_issue_spec_digest(
    pm: &Pm,
    issue: &str,
    spec: &Path,
    only_opted_in: bool,
) -> Result<Option<String>> {
    let Ok((project, dir)) = write::issue_dir(pm, issue) else {
        // Jobs may name issues not held by this tracker.
        return Ok(None);
    };
    let Ok(canonical) = dir.join("issue.md").canonicalize() else {
        return Ok(None);
    };
    if spec.canonicalize().ok().as_ref() != Some(&canonical)
        || (only_opted_in && context::pin(&project, None).state == "unconfigured")
    {
        return Ok(None);
    }
    let reader = Root::open(&dir)?;
    let source = reader
        .read(
            Path::new("issue.md"),
            crate::context_pack::MAX_COMBINED_BYTES as u64,
        )?
        .ok_or_else(|| Error::rejected("canonical issue spec is unavailable"))?;
    Ok(Some(content_digest(
        parse::split_front(&source)?.1.as_bytes(),
    )))
}

/// Resolve the issue's one live worktree in a declared project repository.
pub fn dispatch_lane(
    verb: &str,
    project: &crate::issue::project::Project,
    front: &crate::issue::model::Front,
) -> Result<PathBuf> {
    let repos = start::declared_repos(project);
    let mut lanes: Vec<PathBuf> = Vec::new();
    for cand in start::open_worktrees(front) {
        let dir = cand.canonicalize().unwrap_or(cand.clone());
        let Ok(root) = crate::worktree::main_root(&dir) else {
            continue;
        };
        if !repos.contains(&root) {
            continue;
        }
        let wt_root = crate::worktree::layout::worktrees_dir(&root)
            .canonicalize()
            .unwrap_or_else(|_| crate::worktree::layout::worktrees_dir(&root));
        if dir.starts_with(&wt_root) {
            lanes.push(dir);
        }
    }
    lanes.sort();
    lanes.dedup();
    match lanes.len() {
        1 => Ok(lanes.remove(0)),
        0 => Err(Error::rejected(format!(
            "{verb}: {} has no open lane the daemon can verify — a dispatch \
             kickoff binds the worktree `issue start` recorded; none of its \
             open worktree refs is a live worktree of the project's repos",
            front.id
        ))),
        n => Err(Error::rejected(format!(
            "{verb}: {} has {n} open lane refs the daemon can verify — \
             which one a kickoff binds is ambiguous; close the stale \
             lanes first",
            front.id
        ))),
    }
}

/// Prepare a native job context only after resolving the captured issue's
/// unique open lane and matching both the job repository and task worktree.
/// The scope check runs lazily, after the project and endpoint opt-in gates.
pub fn admitted_job_context(
    pm: &Pm,
    gated: GatedIssue,
    plan: &store::DispatchPlan,
    no_lessons: bool,
) -> Result<Option<PreparedContext<'static>>> {
    if gated.dir != pm.dir.join(&gated.project.key).join(&gated.front.id) {
        return Err(Error::rejected(
            "context refused: the captured issue belongs to a different tracker — nothing was queued",
        ));
    }
    let job = &plan.job;
    let task = &plan.task;
    let scope = |project: &crate::issue::project::Project,
                 front: &crate::issue::model::Front|
     -> Result<()> {
        let lane = dispatch_lane("job dispatch", project, front)?;
        let lane_root = crate::worktree::main_root(&lane).map_err(|error| {
            crate::context_pack::Refusal::ScopeConflict {
                detail: format!(
                    "issue {}'s lane {} has no proven repository: {error}",
                    front.id,
                    lane.display()
                ),
            }
            .error()
        })?;
        let repo = job.repo.as_deref().ok_or_else(|| {
            crate::context_pack::Refusal::ScopeConflict {
                detail: format!("issue-linked job '{}' names no repository", job.id),
            }
            .error()
        })?;
        let repo = Path::new(repo).canonicalize().map_err(|error| {
            crate::context_pack::Refusal::ScopeConflict {
                detail: format!(
                    "job '{}' repository {repo} cannot be resolved: {error}",
                    job.id
                ),
            }
            .error()
        })?;
        if repo != lane_root {
            return Err(crate::context_pack::Refusal::ScopeConflict {
                detail: format!(
                    "job '{}' names repository {}, but issue {}'s lane {} is in {}",
                    job.id,
                    repo.display(),
                    front.id,
                    lane.display(),
                    lane_root.display()
                ),
            }
            .error());
        }
        let worktree = task.worktree.as_deref().ok_or_else(|| {
            crate::context_pack::Refusal::ScopeConflict {
                detail: format!("issue-linked task '{}' binds no worktree", task.id),
            }
            .error()
        })?;
        let task_lane = crate::worktree::layout::worktree_dir(&lane_root, worktree);
        let task_lane = task_lane.canonicalize().map_err(|error| {
            crate::context_pack::Refusal::ScopeConflict {
                detail: format!(
                    "task '{}' worktree {} cannot be resolved: {error}",
                    task.id,
                    task_lane.display()
                ),
            }
            .error()
        })?;
        if task_lane != lane {
            return Err(crate::context_pack::Refusal::ScopeConflict {
                detail: format!(
                    "task '{}' binds worktree {}, not issue {}'s verified lane {}",
                    task.id,
                    task_lane.display(),
                    front.id,
                    lane.display()
                ),
            }
            .error());
        }
        Ok(())
    };
    job_context(
        pm,
        gated,
        JobContext {
            job,
            task,
            revision: plan.revision,
            message: &plan.message_id,
            target: super::ContextTarget {
                team_role: plan.worker.team_role.as_deref(),
                provider: &plan.worker.provider,
                endpoint_kind: &plan.worker.endpoint_kind,
            },
            admission_scope: Some(&scope),
        },
        no_lessons,
    )
}

/// CAD-1846: the same mandatory pipeline for an issue-linked job task.
/// The caller proves the job's issue binding and real lane from its own
/// captured rows; this captures the complete effective spec once, the
/// parent issue's complete prose and the task's own acceptance, and
/// builds the contract at the job task's identity. `None` means the
/// project has not opted in — the caller renders the legacy kickoff and
/// never labels it a complete contract.
pub(crate) fn job_context(
    pm: &Pm,
    gated: GatedIssue,
    job: JobContext<'_>,
    no_lessons: bool,
) -> Result<Option<PreparedContext<'static>>> {
    let GatedIssue {
        project,
        dir,
        front,
        items: parent_items,
        source,
        request: parent_request,
    } = gated;
    let JobContext {
        job: job_row,
        task,
        revision,
        message,
        target,
        admission_scope,
    } = job;
    if task.job_id != job_row.id || job_row.issue_id.as_deref() != Some(front.id.as_str()) {
        return Err(Error::rejected(
            "context refused: the job task is not bound to this issue — nothing was queued",
        ));
    }
    let provider = target.provider;
    let endpoint_kind = target.endpoint_kind;
    context_guard(pm, &project, &front, target, no_lessons, || {
        // The caller's own scope proof runs only after this project is
        // confirmed opted in and its endpoint can receive context, and
        // before any spec byte is read: an opt-out dispatch never pays for
        // a lane proof, and its legacy kickoff needs none.
        if let Some(check) = admission_scope {
            check(&project, &front)?;
        }
        // The complete effective spec, approved and captured once. The
        // canonical locator must be the task's own lane, the captured issue
        // directory, or a tracked file of a declared project repository; the
        // bytes come through the descriptor-relative bounded reader, never a
        // daemon-cwd path or an unbounded fs::read.
        let spec_path = task
            .spec_path
            .as_deref()
            .unwrap_or(job_row.spec_path.as_str());
        let (captured_spec, spec_file, spec_scope) =
            read_job_spec(&project, &dir, task, job_row, spec_path)?;
        let issue_file = std::fs::canonicalize(dir.join("issue.md")).ok();
        let canonical_issue_spec = issue_file.as_deref() == Some(spec_file.as_path());
        // Use the selected issue snapshot for both provenance and its request,
        // not a second source revision observed during the scope/read proof.
        let spec = if canonical_issue_spec {
            source.as_str()
        } else {
            captured_spec.as_str()
        };
        let spec_digest = content_digest(spec.as_bytes());
        // The job's recorded digest still gates its own spec. The working
        // bytes may be dirty; this digest is the captured data revision, not
        // a claim that the file matches HEAD.
        if task.spec_path.is_none() {
            if let Some(expected) = job_row.spec_sha256.as_deref() {
                let canonical_digest = if canonical_issue_spec {
                    Some(content_digest(parse::split_front(spec)?.1.as_bytes()))
                } else {
                    None
                };
                if canonical_digest.as_deref().unwrap_or(&spec_digest) != expected {
                    return Err(Error::rejected(format!(
                        "context refused: spec-changed: the task spec {} no longer \
                         matches the revision the job was opened with — reopen the task or \
                         restore the pinned spec; nothing was queued",
                        spec_file.display()
                    )));
                }
            }
        }
        // Do not re-embed tracker frontmatter or criteria already represented
        // by the native task. A task-specific checklist may differ from the
        // canonical spec's checklist, whose other requirements remain mandatory.
        let spec_is_issue = canonical_issue_spec || spec.as_bytes() == source.as_bytes();
        let acceptance = task_acceptance(task);
        let title = task
            .title
            .as_deref()
            .or(job_row.title.as_deref())
            .unwrap_or(front.title.as_str());
        // The identity contract first, so its parent source digest is the
        // captured issue SHA that both provenance and the composite use
        // without hashing the issue source a second time.
        let mut contract = crate::context_pack::TaskContract::new(
            &task.id,
            title,
            source.as_bytes(),
            &acceptance,
            String::new(),
        );
        contract.issue = front.id.clone();
        let parent_revision = contract.revision.clone();
        let mut request = String::with_capacity(
            spec.len() + parent_request.len() + spec_file.as_os_str().len() + 192,
        );
        // Provenance first: the approved canonical locator, the exact
        // captured content digest and the approved scope. This is the data
        // revision of the working bytes — never a fabricated Git-verified SHA.
        request.push_str(&format!(
            "Task spec source: {} (sha256 {}; scope {}; captured working bytes, not a \
             Git-verified revision)\n\n",
            spec_file.display(),
            spec_digest,
            spec_scope,
        ));
        if spec_is_issue {
            request.push_str(&parent_request);
            let mut additional = parent_items
                .iter()
                .filter(|item| !acceptance.iter().any(|task| task.text == item.text))
                .peekable();
            if additional.peek().is_some() {
                if !request.ends_with('\n') {
                    request.push('\n');
                }
                request.push_str("\n## Additional effective spec criteria\n\n");
                for item in additional {
                    request.push_str(if item.checked { "- [x] " } else { "- [ ] " });
                    request.push_str(&item.text);
                    request.push('\n');
                }
            }
        } else {
            request.push_str("## Task spec\n\n");
            request.push_str(spec);
            if !request.ends_with('\n') {
                request.push('\n');
            }
            if !parent_request.trim().is_empty() {
                request.push_str(&format!(
                    "\n## Parent issue {}: {}\n\n",
                    front.id, front.title
                ));
                request.push_str(&parent_request);
                if !request.ends_with('\n') {
                    request.push('\n');
                }
            }
        }
        contract.request = request;
        // The metadata/report envelope, rendered once. Its exact bytes are
        // persisted as the contract's execution so a lost session restores
        // the scope and report/SHA obligation, not only the spec and
        // criteria; the clone is the small necessary copy.
        let body = store::job_kickoff(
            job_row,
            task,
            revision,
            message,
            provider,
            endpoint_kind,
            true,
        )?;
        contract.execution = Some(body.clone());
        // The issue digest alone cannot tell a changed spec, acceptance,
        // title, execution or scope/report envelope from an unchanged one,
        // and a delta keyed on it would leave the previous contract resident.
        // This composite is job-only; the plain path keeps the source SHA.
        let composite = job_contract_digest(&parent_revision, &contract, revision, job_row);
        contract.revision = composite;
        Ok((contract, std::borrow::Cow::Owned(body)))
    })
}

/// The complete acceptance a job task carries, from its own recorded text.
/// A task with no acceptance carries none: the parent issue's criteria are a
/// different contract and are never substituted. A generated CAD-300 listing
/// round-trips exactly; anything else is one free-form criterion whose
/// original bytes are kept (trimming only decides emptiness).
fn task_acceptance(task: &store::Task) -> Vec<AcceptanceItem> {
    let Some(text) = task
        .acceptance
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    else {
        return Vec::new();
    };
    match parse_acceptance_listing(text) {
        Some((items, "")) => items,
        _ => vec![AcceptanceItem {
            text: text.to_string(),
            checked: false,
        }],
    }
}

/// Stream one length-prefixed field into a contract digest. The prefix keeps
/// concatenation unambiguous without serializing or copying the (possibly
/// large) spec/request text merely to hash it.
fn digest_field(hasher: &mut sha2::Sha256, bytes: &[u8]) {
    use sha2::Digest;
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// The job-only contract revision: the captured parent source SHA, the
/// job/task identity and selected native revision, the exact rendered title,
/// request and acceptance, and the exact execution envelope persisted for
/// recovery. The envelope subsumes the scope, provider/report and issue
/// fields, so it is hashed once whole — including a real explicit report
/// message id when the body carries one: restoring that exact obligation
/// after session loss is worth more than excluding delivery ids. A delta
/// keyed on the parent digest alone would leave the previous contract
/// resident whenever only these changed.
fn job_contract_digest(
    parent_revision: &str,
    contract: &crate::context_pack::TaskContract,
    revision: i64,
    job: &store::Job,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    digest_field(&mut hasher, b"cadence-job-contract-v2");
    digest_field(&mut hasher, parent_revision.as_bytes());
    digest_field(&mut hasher, contract.issue.as_bytes());
    digest_field(&mut hasher, contract.task.as_bytes());
    digest_field(&mut hasher, job.id.as_bytes());
    digest_field(&mut hasher, revision.to_string().as_bytes());
    digest_field(&mut hasher, contract.title.as_bytes());
    digest_field(&mut hasher, contract.request.as_bytes());
    digest_field(
        &mut hasher,
        &(contract.acceptance.len() as u64).to_le_bytes(),
    );
    for item in &contract.acceptance {
        digest_field(&mut hasher, item.as_bytes());
    }
    digest_field(
        &mut hasher,
        contract.execution.as_deref().unwrap_or_default().as_bytes(),
    );
    format!("{:x}", hasher.finalize())
}
