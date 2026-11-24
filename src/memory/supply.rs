//! CAD-2662: lessons an agent looks up during a task.
//!
//! Dispatch inlines the lessons that apply when a task starts. A lesson that
//! becomes relevant later — the scope changed, a new path is touched — was
//! never seen. This is the same selection, asked for again mid-task: the
//! issue's own match context (component, tags, recorded and planned paths,
//! the assignee's provider and team role), optionally widened by the paths,
//! components and tags the agent is now working on.
//!
//! The project, issue, provider and role are not inputs here. The daemon
//! derives them from the caller's own running turn; only the extra axes come
//! from the request, and they only add selections inside that project.
//! Eligibility, ranking, withholding, redaction and the 12-entry / 4 KiB
//! render caps are the existing dispatch policy — nothing is re-decided.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{areas, board, Pm};

use super::issue_matching::{issue_planned_paths, match_for_issue_ctx};
use super::{render_lessons, semantic_digest, MatchCtx, Matched};

/// The agent event a lookup records. Deliberately not the dispatch
/// `context_supplied` kind: that row carries a pin, manifest and conversation
/// and drives context restore and residency, which a tool lookup must not
/// move. This row carries the same lesson facts a dispatch receipt does.
pub const SUPPLY_EVENT: &str = "lessons_supplied";

/// At most this many extra components or tags per lookup.
pub const AXIS_MAX: usize = 16;
/// Longest extra component or tag.
pub const AXIS_VALUE_MAX: usize = 128;
/// Load-error rows shown per lookup; the count stays whole.
const LIST_MAX: usize = 32;

/// Extra match axes for one lookup, already validated.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Axes {
    pub components: Vec<String>,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
}

impl Axes {
    /// Validate request axes. Paths use the planned-path grammar (repo
    /// relative, no `..`, bounded), so an extra path can never reach outside
    /// the repository a lesson's scope names.
    pub fn new(components: Vec<String>, paths: Vec<String>, tags: Vec<String>) -> Result<Self> {
        for (name, values) in [("components", &components), ("tags", &tags)] {
            if values.len() > AXIS_MAX {
                return Err(Error::rejected(format!(
                    "lesson lookup takes at most {AXIS_MAX} {name}"
                )));
            }
            for value in values {
                if value.trim().is_empty()
                    || value.len() > AXIS_VALUE_MAX
                    || value.chars().any(char::is_control)
                {
                    return Err(Error::rejected(format!(
                        "lesson lookup {name} must be non-empty, at most \
                         {AXIS_VALUE_MAX} bytes, with no control characters"
                    )));
                }
            }
        }
        if paths.len() > areas::PATHS_MAX {
            return Err(Error::rejected(format!(
                "lesson lookup takes at most {} paths",
                areas::PATHS_MAX
            )));
        }
        for path in &paths {
            areas::check_path(path)?;
        }
        Ok(Self {
            components,
            paths,
            tags,
        })
    }
}

/// One lookup's answer and the receipt the daemon records for it.
#[derive(Debug)]
pub struct Supply {
    /// What the agent reads.
    pub response: Value,
    /// The lesson facts of the receipt; the daemon adds the turn binding.
    pub receipt: Value,
}

/// Select and render the lessons that apply to `issue` for this assignee,
/// with `axes` layered on — the same selection dispatch makes.
pub fn supply_for_issue(
    pm: &Pm,
    issue: &board::Issue,
    provider: &str,
    role: Option<&str>,
    axes: &Axes,
) -> Result<Supply> {
    let (matched, ctx, load_errors) =
        match_for_issue_ctx(pm, issue, Some(provider), role, |ctx| {
            ctx.components.extend(axes.components.iter().cloned());
            ctx.paths.extend(axes.paths.iter().cloned());
            ctx.tags.extend(axes.tags.iter().cloned());
        })?;
    let planned = issue_planned_paths(issue);
    Ok(supply_view(
        &issue.project,
        &issue.front.id,
        &matched,
        &ctx,
        &planned,
        &load_errors,
    ))
}

/// Redacted file-controlled text. A redaction failure shows the marker,
/// never the raw value.
fn redacted(text: &str) -> String {
    crate::secret::redact_text(text)
        .unwrap_or_else(|_| crate::context_pack::LESSON_REDACTION_UNAVAILABLE.to_string())
}

/// The response and receipt for one already-matched selection. Supplied
/// lessons are exactly those the renderer emitted; a withheld lesson is its
/// slug and redacted reason only, never its body.
pub(super) fn supply_view(
    project: &str,
    issue: &str,
    matched: &Matched,
    ctx: &MatchCtx,
    planned: &[String],
    load_errors: &[String],
) -> Supply {
    let (text, slugs) = render_lessons(matched);
    let supplied: Vec<Value> = slugs
        .iter()
        .filter_map(|slug| matched.lessons.iter().find(|m| &m.front.id == slug))
        .map(|m| json!({"slug": m.front.id, "type": m.front.kind, "digest": semantic_digest(m)}))
        .collect();
    // Dispatch's own bound for withheld detail: redacted, then clipped, and
    // capped at its entry limit.
    let withheld: Vec<Value> = crate::context_pack::lesson_notes(
        matched
            .withheld
            .iter()
            .map(|(m, reason)| (m.front.id.clone(), reason.clone())),
    )
    .into_iter()
    .map(|note| json!({"slug": note.id, "reason": note.reason}))
    .collect();
    let errors: Vec<String> = load_errors
        .iter()
        .take(LIST_MAX)
        .map(|e| redacted(e))
        .collect();
    let omitted = matched.lessons.len().saturating_sub(supplied.len());
    let context = json!({
        "components": ctx.components,
        "paths": ctx.paths,
        "planned_paths": planned,
        "providers": ctx.providers,
        "tags": ctx.tags,
        "roles": ctx.roles,
    });
    let lessons_digest = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(text.as_bytes()))
    };
    let response = json!({
        "project": project,
        "issue": issue,
        "lessons": text,
        "supplied": supplied,
        "omitted": omitted,
        "withheld": withheld,
        "withheld_total": matched.withheld.len(),
        "load_errors": errors,
        "context": context,
    });
    let receipt = json!({
        "project": project,
        "issue": issue,
        "supplied": supplied,
        "omitted": omitted,
        "withheld": withheld,
        "withheld_total": matched.withheld.len(),
        "load_errors": load_errors.len(),
        "context": context,
        "lessons_bytes": text.len(),
        "lessons_digest": lessons_digest,
    });
    Supply { response, receipt }
}

/// Signed lessons on disk for CAD-2662 tests: an accepted record with two
/// distinct worker reviews and a PM finalization over its own digest, so
/// retrieval qualifies it exactly as it would a reviewed lesson.
#[cfg(any(test, all(debug_assertions, feature = "test-seam")))]
#[cfg_attr(not(test), doc(hidden))]
#[cfg_attr(not(test), allow(clippy::unwrap_used))]
pub mod fixture {
    use std::path::{Path, PathBuf};

    use super::super::{
        render_memory, retrieval_status, semantic_digest, FinalizationReceipt, Front,
        IdentityProof, Memory, ReviewReceipt, Scope,
    };

    fn proof(alias: &str, registration: u64) -> IdentityProof {
        IdentityProof {
            alias: alias.to_string(),
            registration,
            generation: format!("gen-{registration}"),
            process_start: registration + 100,
            role: if alias == "pm" { "pm" } else { "worker" }.to_string(),
            uid: None,
        }
    }

    fn review(actor: &IdentityProof, digest: &str) -> ReviewReceipt {
        ReviewReceipt {
            reviewer: actor.alias.clone(),
            identity: actor.stable_id(),
            generation: actor.generation.clone(),
            process_start: actor.process_start,
            role: actor.role.clone(),
            operation: "accept".to_string(),
            cycle: 1,
            digest: digest.to_string(),
            verdict: "pass".to_string(),
            evidence: "independent evidence".to_string(),
            recorded_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// An accepted, retrievable lesson of `project`. `body` is its Markdown
    /// body and `scope` what it applies to; all three are signed.
    pub fn signed_lesson(project: &str, id: &str, body: &str, scope: Scope) -> Memory {
        let author = proof("author", 1);
        let mut mem = Memory {
            project: project.to_string(),
            front: Front {
                id: id.to_string(),
                kind: "rule".to_string(),
                status: "accepted".to_string(),
                scope,
                source: Some("CAD-2662".to_string()),
                confidence: "medium".to_string(),
                created: "2026-01-01T00:00:00Z".to_string(),
                verified_at: None,
                stale: None,
                supersedes: None,
                author: Some(author.alias.clone()),
                author_proof: Some(author),
                contributors: Vec::new(),
                review_cycle: 1,
                active_operation: None,
                reviews: Vec::new(),
                finalizations: Vec::new(),
                policy: None,
            },
            body: body.to_string(),
            path: PathBuf::new(),
        };
        let digest = semantic_digest(&mem);
        mem.front
            .reviews
            .push(review(&proof("worker-a", 2), &digest));
        mem.front
            .reviews
            .push(review(&proof("worker-b", 3), &digest));
        mem.front.finalizations.push(FinalizationReceipt {
            operation: "accept".to_string(),
            cycle: 1,
            digest: digest.clone(),
            finalizer: proof("pm", 4),
            finalized_at: "2026-01-01T00:00:00Z".to_string(),
        });
        assert!(retrieval_status(&mem).0, "{}", retrieval_status(&mem).1);
        mem
    }

    /// A project with an empty memory folder under `pm_dir`.
    pub fn project(pm_dir: &Path, key: &str, prefix: &str) {
        let dir = pm_dir.join(key);
        std::fs::create_dir_all(dir.join("memory")).unwrap();
        std::fs::write(
            dir.join("project.yaml"),
            format!("key: {key}\nprefix: {prefix}\ncomponents: []\n"),
        )
        .unwrap();
    }

    /// Write `mem` into `key`'s memory folder.
    pub fn write(pm_dir: &Path, key: &str, mem: &Memory) {
        std::fs::write(
            pm_dir
                .join(key)
                .join("memory")
                .join(format!("{}.md", mem.front.id)),
            render_memory(&mem.front, &mem.body).unwrap(),
        )
        .unwrap();
    }

    /// The fixture both CAD-2662 suites use: `alpha` holds a project-wide
    /// lesson, a stale one and one scoped to `src/adapter/**`; `beta` holds
    /// a project-wide lesson that must never reach an `alpha` caller.
    pub fn two_projects(pm_dir: &Path) {
        project(pm_dir, "alpha", "A");
        project(pm_dir, "beta", "B");
        let body = |marker: &str| format!("{marker}\n\n**Why:** e\n\n**How to apply:** a\n");
        let wide = Scope {
            project: true,
            ..Scope::default()
        };
        write(
            pm_dir,
            "alpha",
            &signed_lesson(
                "alpha",
                "alpha-wide",
                &body("ALPHA_WIDE_FACT"),
                wide.clone(),
            ),
        );
        let mut stale = signed_lesson(
            "alpha",
            "alpha-stale",
            &body("ALPHA_STALE_BODY"),
            wide.clone(),
        );
        stale.front.stale = Some("CAD-9 reverted the fix".into());
        write(pm_dir, "alpha", &stale);
        write(
            pm_dir,
            "alpha",
            &signed_lesson(
                "alpha",
                "alpha-adapter",
                &body("ALPHA_ADAPTER_FACT"),
                Scope {
                    paths: vec!["src/adapter/**".into()],
                    ..Scope::default()
                },
            ),
        );
        write(
            pm_dir,
            "beta",
            &signed_lesson("beta", "beta-wide", &body("BETA_WIDE_FACT"), wide),
        );
    }
}
