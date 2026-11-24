//! Context-pack admission guard and accepted-lesson selection for `issue::dispatch`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};

use super::{check_body, DispatchArgs};
use crate::error::Result;
use crate::issue::model::Front;
use crate::issue::{board, Pm};
use crate::memory;
use crate::store;

/// The runtime facts that pick a task's context role and delivery
/// envelope. They come from the actual assigned worker row, never from a
/// request field, a project default or a guessed model.
pub struct ContextTarget<'a> {
    pub team_role: Option<&'a str>,
    pub provider: &'a str,
    pub endpoint_kind: &'a str,
}

/// A validated opted-in task context: the manifest the admission
/// transaction must pin, and the exact body that turn delivers.
pub struct PreparedContext<'a> {
    pub manifest: crate::context_pack::Manifest,
    pub body: std::borrow::Cow<'a, str>,
}

/// One job task's dispatch snapshot: the store-captured rows, the
/// proposed dispatch revision and message id, the actual assignee's
/// runtime facts and — for a native admission — the caller's proof of the
/// issue lane the job actually holds.
#[doc(hidden)]
pub struct JobContext<'a> {
    pub job: &'a store::Job,
    pub task: &'a store::Task,
    pub revision: i64,
    pub message: &'a str,
    pub target: ContextTarget<'a>,
    /// The native caller's scope proof for `(project, front)`: the exact
    /// issue lane, the job's declared repo and the task's lane must agree
    /// before any spec byte enters the contract. `None` is a preview (the
    /// preflight of a dispatch whose lane does not exist yet) and is never
    /// an admission; no fake proof stands in for it.
    pub admission_scope: Option<AdmissionScope<'a>>,
}

/// The native caller's issue-lane proof, run once inside the guard's lazy
/// factory — after opt-in and endpoint confirmation, before any spec read.
#[doc(hidden)]
pub type AdmissionScope<'a> =
    &'a dyn Fn(&crate::issue::project::Project, &Front) -> crate::error::Result<()>;

/// Synthetic identity of the optional lesson source and its aggregate gap
/// facts. Nothing is read from this path: it names the source family in a
/// manifest/receipt and keeps lesson gaps distinguishable from repository
/// documents. Provenance is deliberately one fact per category — a count and
/// a representative reason — so a large memory pool can never turn optional
/// metadata into a mandatory-overflow refusal.
const LESSON_SOURCE_ID: &str = "native:lessons";
pub(super) const LESSON_KIND: &str = "lessons";
pub(super) const LESSON_PATH: &str = "memory://accepted-lessons";
const LESSON_WITHHELD_ID: &str = "native:lessons-withheld";
const LESSON_LOAD_ID: &str = "native:lessons-load";
pub(super) const LESSON_OMITTED_ID: &str = "native:lessons-omitted";

/// One aggregate lesson fact stays this small. A memory record's own text
/// (for example a `stale:` note) is file-controlled, so it can never be
/// copied whole into mandatory metadata: the optional lesson source carries
/// the renderer-capped detail, and this fact names the count and a bounded
/// reason.
const LESSON_FACT_MAX: usize = 480;

fn lesson_fact(text: &str) -> String {
    if text.len() <= LESSON_FACT_MAX {
        return text.to_string();
    }
    let mut end = LESSON_FACT_MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

pub(super) fn content_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Redacted, bounded file-controlled lesson text: a withholding reason, a
/// memory load-error line (an unreadable file name can be token-shaped) or a
/// match failure. Redaction runs before the cap, and a redaction failure shows
/// the marker — never the raw value.
pub(super) fn redacted_fact(text: &str) -> String {
    let clean = match crate::secret::redact_text(text) {
        Ok(text) => text,
        Err(_) => crate::context_pack::LESSON_REDACTION_UNAVAILABLE.to_string(),
    };
    lesson_fact(&clean)
}

/// Select the project-memory lessons current policy applies to this issue
/// and the actual assignee's provider, exactly once. The rendered Markdown
/// becomes one optional content-addressed source whose included IDs are the
/// entries the renderer actually emitted; withheld, load-error and omitted
/// counts stay visible as bounded aggregate facts. Eligibility, ranking,
/// withholding and the render caps are the existing memory policy; nothing
/// here re-verifies or re-renders.
fn lesson_selection(
    pm: &Pm,
    project: &crate::issue::project::Project,
    front: &Front,
    provider: &str,
    role: Option<&str>,
) -> crate::context_pack::LessonSelection {
    let issue = board::Issue {
        project: project.key.clone(),
        dir: pm.dir.join(&project.key).join(&front.id),
        front: front.clone(),
        body: String::new(),
        comments: vec![],
        artifacts: vec![],
    };
    let gap = |id: &str, state: &str, reason: String| crate::context_pack::Gap {
        id: id.to_string(),
        path: LESSON_PATH.to_string(),
        state: state.to_string(),
        reason,
        required: false,
        lessons: Vec::new(),
    };
    let mut withheld = Vec::new();
    let source = match memory::match_for_issue(pm, &issue, Some(provider), role) {
        Err(error) => {
            withheld.push(gap(
                LESSON_SOURCE_ID,
                "unavailable",
                redacted_fact(&format!("memory match failed: {error}")),
            ));
            None
        }
        Ok((matched, errors)) => {
            if !matched.withheld.is_empty() {
                // The aggregate fact names the complete count; bounded detail
                // names each lesson with its recorded reason. Neither enters
                // the mandatory payload — the optional lesson source carries
                // the renderer-capped text, and both facts ride the pin and
                // supply receipts.
                let detail = crate::context_pack::lesson_notes(
                    matched
                        .withheld
                        .iter()
                        .map(|(memory, reason)| (memory.front.id.clone(), reason.clone())),
                );
                let shown = detail.len();
                withheld.push(crate::context_pack::Gap {
                    id: LESSON_WITHHELD_ID.to_string(),
                    path: LESSON_PATH.to_string(),
                    state: "withheld".to_string(),
                    reason: format!(
                        "{} in-scope lesson(s) withheld by validity or role policy; \
                         `cadence memory match --issue {} --json` lists reasons{}",
                        matched.withheld.len(),
                        front.id,
                        if shown < matched.withheld.len() {
                            format!("; first {shown} named here")
                        } else {
                            String::new()
                        },
                    ),
                    required: false,
                    lessons: detail,
                });
            }
            if let Some(line) = memory::load_errors_line(&errors) {
                // The first failing file name is file-controlled and can be
                // token-shaped; redact before bounding the gap reason.
                withheld.push(gap(LESSON_LOAD_ID, "unreadable", redacted_fact(&line)));
            }
            let (text, slugs) = memory::render_lessons(&matched);
            // Omitted reporting does not depend on the renderer's header: a
            // matched lesson the renderer did not emit is named (bounded) even
            // when no lesson text shipped at all.
            let omitted = matched.lessons.len().saturating_sub(slugs.len());
            if omitted > 0 {
                // The emitted slugs are the renderer's truth; everything
                // else was dropped by its entry or byte cap.
                let emitted: std::collections::HashSet<&str> =
                    slugs.iter().map(String::as_str).collect();
                let detail = crate::context_pack::lesson_notes(
                    matched
                        .lessons
                        .iter()
                        .filter(|memory| !emitted.contains(memory.front.id.as_str()))
                        .map(|memory| {
                            (
                                memory.front.id.clone(),
                                "matched lesson omitted by the bounded lesson injection cap"
                                    .to_string(),
                            )
                        }),
                );
                let shown = detail.len();
                withheld.push(crate::context_pack::Gap {
                    id: LESSON_OMITTED_ID.to_string(),
                    path: LESSON_PATH.to_string(),
                    state: "overflow".to_string(),
                    reason: format!(
                        "{omitted} matched lesson(s) omitted by the lesson injection cap{}",
                        if shown < omitted {
                            format!("; first {shown} named here")
                        } else {
                            String::new()
                        },
                    ),
                    required: false,
                    lessons: detail,
                });
            }
            if text.is_empty() {
                None
            } else {
                let digest = content_digest(text.as_bytes());
                Some(crate::context_pack::Source {
                    id: LESSON_SOURCE_ID.to_string(),
                    kind: LESSON_KIND.to_string(),
                    path: LESSON_PATH.to_string(),
                    title: "Project lessons".to_string(),
                    required: false,
                    revision: digest.clone(),
                    digest,
                    bytes: text.len(),
                    inline: Some(text),
                    lesson_ids: slugs,
                })
            }
        }
    };
    crate::context_pack::LessonSelection { source, withheld }
}

/// Build, validate and return the manifest a new kickoff's admission
/// transaction will pin. `body` is the exact delivered text; the required
/// sources and that body plus known adapter material must fit the combined
/// cap before anything is admitted.
#[allow(clippy::too_many_arguments)]
fn admit_context<'a>(
    project: &crate::issue::project::Project,
    pinned: crate::issue::context::PinnedContext,
    role: &str,
    assigned: Option<&str>,
    lessons: Option<crate::context_pack::LessonSelection>,
    contract: crate::context_pack::TaskContract,
    body: std::borrow::Cow<'a, str>,
    provider: &str,
) -> Result<Option<PreparedContext<'a>>> {
    let manifest =
        crate::context_pack::build(contract, &project.key, &pinned, role, assigned, lessons)
            .map_err(|refusal| refusal.error())?;
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    let root = crate::issue::context::repository_root(project).map_err(|(state, detail)| {
        crate::context_pack::Refusal::UnavailableProject { state, detail }.error()
    })?;
    let mut reader = |document: &crate::context_pack::Source| {
        crate::issue::context::read_pinned_blob(
            &root,
            &document.revision,
            &document.path,
            &document.digest,
        )
    };
    crate::context_pack::plan(
        &manifest,
        None,
        false,
        body.as_ref(),
        crate::context_pack::adapter_reserved_bytes(provider),
        &mut reader,
    )
    .map_err(|refusal| refusal.error())?;
    Ok(Some(PreparedContext { manifest, body }))
}

/// CAD-1822/CAD-1846: pin and validate a task's context for `target`
/// before a kickoff exists. Shared by [`preflight`] (CLI and
/// `issue_kickoff` HTTP/RPC, before any lane is created) and the daemon's
/// `dispatch_send` (the last gate before enqueue).
///
/// The pin comes first: a project that does not participate returns
/// `None` without calling `make`, so an opt-out dispatch builds no new
/// contract and reads no new source. Selection is daemon-side — the
/// issue's own project, its declared local repository and its tracked
/// manifest at HEAD; no request field can name a source, revision or
/// path. `make` supplies the complete contract and the exact body this
/// turn delivers.
///
/// A required source that cannot be read, an unusable manifest, a
/// combined mandatory payload over the local cap and a non-managed
/// endpoint that cannot receive required prompt context all refuse with
/// [`crate::context_pack::Refusal`] before anything is created.
pub fn context_guard<'a>(
    pm: &Pm,
    project: &crate::issue::project::Project,
    front: &Front,
    target: ContextTarget<'_>,
    no_lessons: bool,
    make: impl FnOnce() -> Result<(crate::context_pack::TaskContract, std::borrow::Cow<'a, str>)>,
) -> Result<Option<PreparedContext<'a>>> {
    let (role, assigned) = crate::context_pack::selection_role(target.team_role);
    let pinned = crate::issue::context::pin(project, Some(role));
    if pinned.state == "unconfigured" {
        return Ok(None);
    }
    if !crate::continuity::endpoint_takes_packs(target.endpoint_kind) {
        return Err(crate::context_pack::Refusal::UnsupportedEndpoint {
            endpoint: target.endpoint_kind.to_string(),
        }
        .error());
    }
    let lessons = (!no_lessons)
        .then(|| lesson_selection(pm, project, front, target.provider, target.team_role));
    let (contract, body) = make()?;
    admit_context(
        project,
        pinned,
        role,
        assigned.as_deref(),
        lessons,
        contract,
        body,
        target.provider,
    )
}

pub(super) struct Lessons {
    pub(super) lessons: Vec<String>,
    pub(super) lessons_file: Option<PathBuf>,
    pub(super) lessons_error: Option<String>,
    pub(super) lessons_withheld: Vec<(String, String)>,
    pub(super) lessons_omitted: Vec<(String, String)>,
    pub(super) body: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lessons(
    mid: &str,
    pm: &Pm,
    project: &crate::issue::project::Project,
    dir: &Path,
    front: &Front,
    args: &DispatchArgs,
    state_dir: &Path,
    provider: &str,
    role: Option<&str>,
    body: Option<String>,
) -> Lessons {
    let mut lessons: Vec<String> = vec![];
    let mut lessons_file: Option<PathBuf> = None;
    let mut lessons_error: Option<String> = None;
    // In-scope guidance withheld by validity or role policy carries its
    // reason in the dispatch comment and output so absence is explainable.
    let mut lessons_withheld: Vec<(String, String)> = vec![];
    let mut body = body;
    if !args.no_lessons {
        if let Some(b) = body.as_mut() {
            let issue_obj = board::Issue {
                project: project.key.clone(),
                dir: dir.to_path_buf(),
                front: front.clone(),
                body: String::new(),
                comments: vec![],
                artifacts: vec![],
            };
            match memory::match_for_issue(pm, &issue_obj, Some(provider), role) {
                Err(e) => lessons_error = Some(format!("memory match failed: {e}")),
                Ok((matched, mem_errors)) => {
                    // Valid records still reach the worker; every file
                    // that failed to load is named on the issue.
                    if let Some(line) = memory::load_errors_line(&mem_errors) {
                        lessons_error = Some(line);
                    }
                    lessons_withheld = matched
                        .withheld
                        .iter()
                        .map(|(m, reason)| (m.front.id.clone(), reason.clone()))
                        .collect();
                    // Written whenever it has content: a dispatch whose
                    // every match was withheld still gets the file with
                    // only its Withheld section.
                    let (text, slugs) = memory::render_lessons(&matched);
                    if !text.is_empty() {
                        let ddir = state_dir.join("dispatch");
                        let file = ddir.join(format!("{mid}-lessons.md"));
                        // tmp + rename: a failed write never leaves a
                        // partial or empty lessons artifact behind.
                        let tmp = ddir.join(format!("{mid}-lessons.tmp"));
                        let prior = b.clone();
                        *b = format!("{prior} Lessons: {}.", file.display());
                        if let Err(e) = check_body(b, provider) {
                            *b = prior;
                            let why =
                                format!("lessons path pushed the kickoff over the body limit: {e}");
                            lessons_error = Some(match lessons_error {
                                Some(prev) => format!("{prev}; {why}"),
                                None => why,
                            });
                        } else if let Err(e) = std::fs::create_dir_all(&ddir)
                            .and_then(|_| std::fs::write(&tmp, &text))
                            .and_then(|_| std::fs::rename(&tmp, &file))
                        {
                            let _ = std::fs::remove_file(&tmp);
                            *b = prior;
                            let why = format!("lessons file unwritable: {e}");
                            lessons_error = Some(match lessons_error {
                                Some(prev) => format!("{prev}; {why}"),
                                None => why,
                            });
                        } else {
                            lessons = slugs;
                            lessons_file = Some(file);
                        }
                    }
                }
            }
        }
    }
    Lessons {
        lessons,
        lessons_file,
        // Every legacy error here can carry a file-controlled memory name
        // (load-error line, match failure, lessons path); redact once at the
        // construction boundary, before any output sink bounds it.
        lessons_error: lessons_error.as_deref().map(redacted_fact),
        lessons_withheld,
        // The legacy sidecar path has no pinned selection to compare against,
        // so it reports no omitted detail.
        lessons_omitted: vec![],
        body,
    }
}
