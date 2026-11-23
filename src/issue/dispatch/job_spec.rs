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

/// Resolve a spec locator against the same project and task bases used by
/// the scoped reader. A relative job locator is never relative to the daemon.
fn spec_candidate(
    declared: &[PathBuf],
    task: &store::Task,
    job: &store::Job,
    spec_path: &str,
) -> Result<PathBuf> {
    let issue_id = job.issue_id.as_deref().unwrap_or_default();
    let raw = Path::new(spec_path);
    if raw.is_absolute() {
        return Ok(raw.to_path_buf());
    }
    let base = if task.spec_path.is_some() {
        task_spec_base(declared, task.worktree.as_deref(), issue_id)
    } else {
        default_spec_base(declared, job.repo.as_deref())
    };
    let Some(base) = base else {
        return Err(spec_scope_error(
            spec_path,
            "a relative spec has no approved task lane or declared repository",
        ));
    };
    Ok(base.join(raw))
}

/// Resolve a relative job locator to the same canonical file used by the
/// scoped kickoff reader. Absolute legacy locators remain untouched.
pub(crate) fn intake_spec_pin(
    project: &crate::issue::project::Project,
    task: &store::Task,
    job: &store::Job,
) -> Result<PathBuf> {
    let raw = Path::new(&job.spec_path);
    if raw.is_absolute() {
        return Ok(raw.to_path_buf());
    }
    let candidate = spec_candidate(&start::declared_repos(project), task, job, &job.spec_path)?;
    Ok(candidate.canonicalize().unwrap_or(candidate))
}

/// Re-read a canonical relative-job pin without following replaced symlinks
/// or reading more than the context reader's bound.
pub(crate) fn intake_spec_matches(path: &Path, expected: &str) -> bool {
    let Ok(relative) = path.strip_prefix("/") else {
        return false;
    };
    Root::open(Path::new("/"))
        .and_then(|root| root.read(relative, crate::context_pack::MAX_COMBINED_BYTES as u64))
        .ok()
        .flatten()
        .is_some_and(|text| content_digest(text.as_bytes()) == expected)
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
    let candidate = spec_candidate(&declared, task, job, spec_path)?;
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

/// Capture a repo-relative default job spec for an intake cloud kickoff
/// without granting the locator authority to read outside the approved scope.
/// Canonical issue specs use the approved issue snapshot, not their old raw
/// digest; independent specs must still match the job's immutable byte pin.
pub(crate) fn capture_intake_cloud_spec(
    gated: &GatedIssue,
    task: &store::Task,
    job: &store::Job,
    bound_spec: Option<&Path>,
) -> Result<String> {
    let (text, file, _) = read_job_spec(&gated.project, &gated.dir, task, job, &job.spec_path)?;
    if bound_spec.is_some_and(|expected| expected != file.as_path()) {
        return Err(Error::rejected(
            "task dispatch refused: independent spec changed while preparing the kickoff",
        ));
    }
    let canonical_issue = gated
        .dir
        .join("issue.md")
        .canonicalize()
        .ok()
        .is_some_and(|issue| issue == file);
    if canonical_issue {
        return Ok(gated.source.clone());
    }
    let expected = job.spec_sha256.as_deref().ok_or_else(|| {
        Error::rejected(
            "task dispatch refused: a relative independent intake cloud spec has no immutable digest",
        )
    })?;
    if content_digest(text.as_bytes()) != expected {
        return Err(Error::rejected(format!(
            "context refused: spec-changed: the task spec {} no longer matches the revision \
             the job was opened with — reopen the task or restore the pinned spec; nothing was queued",
            file.display()
        )));
    }
    Ok(text)
}

/// Canonical issue specs contain mutable tracker metadata that is not part
/// of their effective spec. Pin every body byte, including all criteria;
/// retain the separate raw source digest in the delivery manifest.
pub(crate) fn canonical_issue_spec_digest(
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

/// CAD-1846: the mandatory pipeline for an issue-linked job task. The caller
/// proves its captured issue binding and lane. Intake admissions supply the
/// approved issue checklist; other tasks keep their recorded acceptance.
/// `None` means the project has not opted in and the caller renders the
/// legacy kickoff instead.
pub(crate) fn job_context(
    pm: &Pm,
    gated: &GatedIssue,
    job: JobContext<'_>,
    no_lessons: bool,
    approved_issue_items: Option<&[AcceptanceItem]>,
    bound_spec: Option<&Path>,
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
    context_guard(pm, project, front, target, no_lessons, || {
        // The caller's own scope proof runs only after this project is
        // confirmed opted in and its endpoint can receive context, and
        // before any spec byte is read: an opt-out dispatch never pays for
        // a lane proof, and its legacy kickoff needs none.
        if let Some(check) = admission_scope {
            check(project, front)?;
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
            read_job_spec(project, dir, task, job_row, spec_path)?;
        if bound_spec.is_some_and(|expected| expected != spec_file.as_path()) {
            return Err(Error::rejected(
                "task dispatch refused: independent spec changed while preparing the kickoff",
            ));
        }
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
        // Independent specs and ordinary jobs retain their byte pin. For an
        // intake's canonical issue file the admitted scope supersedes the old
        // raw body digest, which can change when a criterion is checked off.
        if task.spec_path.is_none() && !(canonical_issue_spec && approved_issue_items.is_some()) {
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
        // Task acceptance is independent of the approved issue scope.
        let spec_is_issue = canonical_issue_spec || spec.as_bytes() == source.as_bytes();
        let mut acceptance = task_acceptance(task);
        if let Some(items) = approved_issue_items {
            acceptance.extend(items.iter().cloned());
        }
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
            request.push_str(parent_request);
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
                request.push_str(parent_request);
                if !request.ends_with('\n') {
                    request.push('\n');
                }
            }
        }
        if approved_issue_items.is_some() {
            let (_, issue_body) = parse::split_front(source)?;
            let scope = parse::acceptance_scope(issue_body)
                .ok_or_else(|| Error::internal("approved intake has no acceptance scope"))?;
            request.push_str("\n## Approved issue acceptance (including qualifiers)\n\n");
            request.push_str(scope);
            request.push('\n');
        }
        if approved_issue_items.is_some() {
            if let Some(recorded) = task.acceptance.as_deref().filter(|s| !s.trim().is_empty()) {
                request.push_str("\n## Independent task acceptance (where compatible)\n\n");
                request.push_str(recorded);
                request.push('\n');
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
