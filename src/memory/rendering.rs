//! Presentation for project memory: the bounded lessons Markdown a
//! dispatch injects and the card/detail JSON `ls` and the UI read.
//! Rendering only — it calls the parent's body parsing, digest, quorum
//! and retrieval-status helpers; no trust decision is made here.

use serde_json::{json, Value};

use super::{
    body_parts, evidence_json, fact_line, last_verified, quorum_status, retrieval_status,
    semantic_digest, Freshness, Matched, Memory,
};

/// Dispatch injection caps — the lessons file stays a quick scan.
pub const LESSON_MAX_ENTRIES: usize = 12;
pub const LESSON_MAX_BYTES: usize = 4 * 1024;

// ── Lessons rendering (dispatch) ─────────────────────────────────

/// Render the lessons file for a dispatch: each entry is the slug, its
/// type and evidence label, the one-line fact and the how-to-apply.
/// Capped at `LESSON_MAX_ENTRIES` memories and `LESSON_MAX_BYTES` total
/// — a truncated tail is noted. Withheld lessons are listed after, with
/// their reasons, inside the same byte cap. Returns `(text, slugs)`.
/// When every match is withheld the text is only the Withheld section
/// — the "why did I not get this?" answer; nothing matched at all
/// yields an empty string.
pub fn render_lessons(matched: &Matched) -> (String, Vec<String>) {
    let mut out = String::from("# Lessons — matched project memories\n\n");
    let mut slugs = Vec::new();
    let mut omitted = 0usize;
    for m in matched.lessons.iter().take(LESSON_MAX_ENTRIES) {
        let (fact, _why, how) = body_parts(&m.body);
        let fact = fact.join(" ").trim().to_string();
        let how = how.lines().next().unwrap_or_default().trim().to_string();
        let entry = format!(
            "- `{}` ({}, {}): {}\n  apply: {}\n",
            m.front.id,
            m.front.kind,
            matched.label(m),
            fact,
            how
        );
        if out.len() + entry.len() > LESSON_MAX_BYTES {
            omitted += 1;
            continue;
        }
        out.push_str(&entry);
        slugs.push(m.front.id.clone());
    }
    let extra = matched.lessons.len().saturating_sub(LESSON_MAX_ENTRIES) + omitted;
    if extra > 0 {
        out.push_str(&format!(
            "\n({extra} more matched — `cadence memory ls` lists them)\n"
        ));
    }
    if slugs.is_empty() && matched.withheld.is_empty() {
        return (String::new(), vec![]);
    }
    if !matched.withheld.is_empty() {
        out.push_str("\n## Withheld — stale evidence, not applied\n\n");
        for (m, reason) in &matched.withheld {
            let entry = format!("- `{}`: {reason}\n", m.front.id);
            if out.len() + entry.len() > LESSON_MAX_BYTES {
                break;
            }
            out.push_str(&entry);
        }
    }
    (out, slugs)
}

/// Card/list payload for `ls` and the UI. `evidence` is the lesson's
/// freshness under `fresh` (its project's window), as retrieval reads it.
pub fn card_json(m: &Memory, fresh: &Freshness) -> Value {
    let digest = semantic_digest(m);
    let (eligible, reason) = retrieval_status(m);
    let (accept_eligible, accept_reason) = quorum_status(m, "accept");
    let (verify_eligible, verify_reason) = quorum_status(m, "verify");
    json!({
        "project": m.project,
        "slug": m.front.id,
        "type": m.front.kind,
        "status": m.front.status,
        "confidence": m.front.confidence,
        "scope": {
            "project": m.front.scope.project,
            "components": m.front.scope.components,
            "paths": m.front.scope.paths,
            "providers": m.front.scope.providers,
            "tags": m.front.scope.tags,
        },
        "source": m.front.source,
        "author": m.front.author,
        "created": m.front.created,
        "verified_at": m.front.verified_at,
        "last_verified": last_verified(m),
        "stale": m.front.stale,
        "evidence": evidence_json(m, fresh),
        "supersedes": m.front.supersedes,
        "revision_digest": digest,
        "review_cycle": m.front.review_cycle,
        "active_operation": m.front.active_operation,
        "review_count": m.front.reviews.len(),
        "finalization_count": m.front.finalizations.len(),
        "finalized_operations": m.front.finalizations.iter().map(|r| json!({
            "operation": r.operation.clone(),
            "cycle": r.cycle,
            "digest": r.digest.clone(),
            "finalized_at": r.finalized_at.clone(),
            "finalizer": r.finalizer.alias.clone(),
        })).collect::<Vec<_>>(),
        "quorum": {
            "eligible": eligible,
            "reason": reason,
            "accept": {"eligible": accept_eligible, "reason": accept_reason},
            "verify": {"eligible": verify_eligible, "reason": verify_reason},
        },
        "fact": fact_line(&m.body),
        "path": m.path,
    })
}

pub fn detail_json(m: &Memory, fresh: &Freshness) -> Value {
    let mut v = card_json(m, fresh);
    v["body"] = json!(m.body);
    v
}
