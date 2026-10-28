//! Presentation for project memory: the bounded lessons Markdown a
//! dispatch injects and the card/detail JSON `ls` and the UI read.
//! Rendering only — it calls the parent's body parsing, digest, quorum
//! and retrieval-status helpers; no trust decision is made here.
//!
//! The lessons file adds three display-only projections: recorded
//! applicability, recorded citations and a declared check condition. Each is
//! redacted before display and fails closed to a marker, and none is a trust
//! decision — no citation is resolved, no cited content is read and no check
//! runs. A wiki reference is shown whole only when `wiki::normalize` accepts
//! it, it names `global/` or the lesson's own `projects/<key>/` namespace, and
//! the wiki's own read table grants the weakest reader (`Caller::Public`)
//! access. Every other reference is withheld with its count and reason: a
//! stored reference is not proof that the worker may see that target, and
//! agent-memory areas or another project's pages are never exposed on the
//! strength of the coarse read table alone. A withheld stale reason is
//! redacted for the same non-disclosure reason as the display fields.

use serde_json::{json, Value};

use super::{
    body_parts, evidence_json, fact_line, last_verified, policy, quorum_status, retrieval_status,
    semantic_digest, Freshness, Matched, Memory,
};

/// Dispatch injection caps — the lessons file stays a quick scan.
pub const LESSON_MAX_ENTRIES: usize = 12;
pub const LESSON_MAX_BYTES: usize = 4 * 1024;

/// What a reader must not infer from the entries that follow.
const LESSON_HEADER: &str = "# Lessons — matched project memories\n\n\
    Cited references are recorded pointers, not resolved evidence; a declared \
    check condition is advisory and never run here.\n\n";

/// Per-entry display caps. Applicability, citations and a condition orient
/// the reader; they never push an entry or the file past its bound.
const APPLICABILITY_MAX_CHARS: usize = 160;
const CHECK_ID_MAX_CHARS: usize = 64;
/// The one reason a reference is withheld: this render path carries no
/// recipient reader, so the target's access cannot be established.
const ACCESS_UNPROVEN: &str = "target access not established";
/// Bytes held back while listing withheld entries, so a tail cut at the cap
/// is always named instead of disappearing. Covers the longest count line
/// with room to spare.
const WITHHELD_NOTE_BUDGET: usize = 96;
const WITHHELD_HEADER: &str = "\n## Withheld — not applied\n\n";

// ── Bounded display helpers ──────────────────────────────────────

/// Redacted display text at full length — for values whose exact bytes
/// matter, such as an immutable revision.
fn redacted_text(raw: &str) -> String {
    match crate::secret::redact_text(raw) {
        Ok(text) => text,
        Err(_) => "[redaction unavailable]".to_string(),
    }
}

/// Redacted, clipped display text. Redaction runs first so a credential is
/// never shortened into a shape no rule matches; a redaction failure shows
/// a marker rather than the raw value.
fn display_text(raw: &str, max_chars: usize) -> String {
    clip(redacted_text(raw).trim(), max_chars)
}

/// Char-boundary-safe clip; the trailing marker says the value was shortened.
fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The kind label a citation line shows.
fn citation_kind(kind: &policy::EvidenceKind) -> &'static str {
    match kind {
        policy::EvidenceKind::Git => "git",
        policy::EvidenceKind::Wiki => "wiki",
        policy::EvidenceKind::NativeRecord => "native",
        policy::EvidenceKind::External => "external",
    }
}

/// Tally one withheld reference under its kind.
fn count_withheld(withheld: &mut Vec<(&'static str, usize)>, kind: &'static str) {
    match withheld.iter_mut().find(|(seen, _)| *seen == kind) {
        Some((_, count)) => *count += 1,
        None => withheld.push((kind, 1)),
    }
}

/// What the lesson applies to, as recorded: its scope axes plus the policy
/// role gate. This is the recorded applicability, not a claim about why the
/// current dispatch selected it.
fn applicability_line(m: &Memory) -> String {
    let scope = &m.front.scope;
    let mut parts = Vec::new();
    if scope.project {
        parts.push("project-wide".to_string());
    }
    if !scope.components.is_empty() {
        parts.push(format!("components {}", scope.components.join(", ")));
    }
    if !scope.paths.is_empty() {
        parts.push(format!("paths {}", scope.paths.join(", ")));
    }
    if !scope.providers.is_empty() {
        parts.push(format!("providers {}", scope.providers.join(", ")));
    }
    if !scope.tags.is_empty() {
        parts.push(format!("tags {}", scope.tags.join(", ")));
    }
    if let Some(record) = &m.front.policy {
        if record.audience != policy::Audience::Project || !record.roles.is_empty() {
            let audience = match record.audience {
                policy::Audience::Project => "project",
                policy::Audience::Role => "role",
                policy::Audience::Company => "company",
            };
            parts.push(format!("audience {audience}"));
        }
        if !record.roles.is_empty() {
            parts.push(format!("roles {}", record.roles.join(", ")));
        }
    }
    let recorded = if parts.is_empty() {
        "recorded scope is empty".to_string()
    } else {
        parts.join("; ")
    };
    format!(
        "applies: {}",
        display_text(&recorded, APPLICABILITY_MAX_CHARS)
    )
}

/// The normalized wiki path when it may be shown to a dispatch recipient.
///
/// Three things must hold, in this order: `wiki::normalize` accepts the
/// stored source (it refuses absolute paths, traversal, dotfiles, `.blob`
/// pointers and control bytes); the path is `global/…` or the lesson's own
/// `projects/<project>/…` namespace — agent view areas and another project's
/// pages stay out even though the coarse table would allow reading them; and
/// `wiki::allowed` grants `Op::Read` to `Caller::Public`, the weakest reader
/// in that table, which proves every recipient may read the path. Nothing is
/// read from the wiki here.
fn readable_wiki_path(source: &str, project: &str) -> Option<String> {
    let path = crate::wiki::normalize(source).ok()?;
    let segments: Vec<&str> = path.split('/').collect();
    let in_scope = match segments.as_slice() {
        ["global", _, ..] => true,
        ["projects", key, _, ..] => *key == project,
        _ => false,
    };
    if !in_scope {
        return None;
    }
    crate::wiki::allowed(
        &crate::wiki::Caller::Public,
        crate::wiki::Op::Read,
        &segments,
    )
    .ok()?;
    Some(path)
}

/// The record's structured citations for the dispatch line — its policy refs
/// plus every attestation's, deduplicated.
///
/// A wiki reference is shown whole (path and full revision, redacted first,
/// never truncated) when [`readable_wiki_path`] proves the recipient's read
/// access. Every other reference is withheld: git resolution runs as the host
/// against the first registered repository, and native/external refs name
/// targets with no read table at all, so none of them proves the worker may
/// see that target. Withheld references are counted by kind with the reason,
/// never dropped as though the list were complete. A reference too long to
/// fit the file bound omits its whole entry instead of being shortened.
fn citation_line(m: &Memory) -> String {
    let Some(record) = &m.front.policy else {
        return "cite: none recorded".to_string();
    };
    let mut unique: Vec<&policy::EvidenceRef> = Vec::new();
    let mut shown: Vec<String> = Vec::new();
    let mut withheld: Vec<(&'static str, usize)> = Vec::new();
    for reference in record.evidence_refs.iter().chain(
        record
            .attestations
            .iter()
            .flat_map(|attestation| attestation.evidence_refs.iter()),
    ) {
        if unique.iter().any(|seen| {
            seen.kind == reference.kind
                && seen.source == reference.source
                && seen.revision == reference.revision
        }) {
            continue;
        }
        unique.push(reference);
        if reference.kind == policy::EvidenceKind::Wiki {
            if let Some(path) = readable_wiki_path(&reference.source, &m.project) {
                shown.push(format!(
                    "wiki {}@{}",
                    redacted_text(&path),
                    redacted_text(&reference.revision)
                ));
                continue;
            }
        }
        count_withheld(&mut withheld, citation_kind(&reference.kind));
    }
    let mut line = String::from("cite: ");
    if !shown.is_empty() {
        line.push_str(&shown.join(", "));
        if !withheld.is_empty() {
            line.push_str("; ");
        }
    }
    if !withheld.is_empty() {
        let counts: Vec<String> = withheld
            .iter()
            .map(|(kind, count)| format!("{count} {kind}"))
            .collect();
        line.push_str(&format!(
            "{} withheld ({})",
            counts.join(", "),
            ACCESS_UNPROVEN
        ));
    } else if shown.is_empty() {
        line.push_str("none recorded");
    }
    line
}

/// The declared check condition, always framed as advisory: a condition is
/// data naming a check and its revisions, never a run or a result. The
/// entry's evidence label carries the verified/unverified state; this line
/// only says whether a condition was recorded, so an absent one is never
/// mistaken for a pass. The Wiki target revision is withheld for the same
/// reason a citation is: no recipient reader exists here to establish access.
fn verification_line(m: &Memory) -> String {
    let condition = m
        .front
        .policy
        .as_ref()
        .and_then(|record| record.verification_condition.as_ref());
    let Some(condition) = condition else {
        return "verify: no condition recorded".to_string();
    };
    let mut line = format!(
        "verify: {}",
        display_text(&condition.check_id, CHECK_ID_MAX_CHARS)
    );
    let source = condition.source_revision.as_deref().map(redacted_text);
    let target = condition.target_revision.as_deref().map(redacted_text);
    match (source, target) {
        (Some(source), Some(target)) => line.push_str(&format!(" {source} -> {target}")),
        (Some(source), None) => line.push_str(&format!(" at {source}")),
        (None, Some(target)) => line.push_str(&format!(" against {target}")),
        (None, None) => {}
    }
    if condition.wiki_target_revision.is_some() {
        line.push_str(&format!("; wiki target withheld ({})", ACCESS_UNPROVEN));
    }
    line.push_str(" (advisory, not run)");
    line
}

// ── Lessons rendering (dispatch) ─────────────────────────────────

/// One whole lesson entry: claim and how-to-apply unchanged from the stored
/// body, then the three new display-only lines — recorded applicability,
/// citations and any declared condition. The entry is rendered in full or
/// not at all — a partial entry would misstate what a lesson says.
fn lesson_entry(m: &Memory, label: &str) -> String {
    let (fact, _why, how) = body_parts(&m.body);
    let fact = fact.join(" ").trim().to_string();
    let how = how.lines().next().unwrap_or_default().trim().to_string();
    format!(
        "- `{}` ({}, {}): {}\n  apply: {}\n  {}\n  {}\n  {}\n",
        m.front.id,
        m.front.kind,
        label,
        fact,
        how,
        applicability_line(m),
        citation_line(m),
        verification_line(m),
    )
}

/// Append a note only while it fits the file cap: the cap is a hard
/// contract, so an oversized note is dropped rather than an entry split.
fn push_note(out: &mut String, note: &str) {
    if out.len() + note.len() <= LESSON_MAX_BYTES {
        out.push_str(note);
    }
}

/// Render the lessons file for a dispatch: each entry is the slug, its
/// type and evidence label, the one-line fact and the how-to-apply, then
/// the recorded applicability, citations and any declared condition. Capped
/// at `LESSON_MAX_ENTRIES` memories and `LESSON_MAX_BYTES` total — entries
/// stay whole and a truncated tail is noted. Withheld lessons are listed
/// after, with their reasons, inside the same byte cap. Returns
/// `(text, slugs)`.
/// When every match is withheld the text is the header and the Withheld
/// section — the "why did I not get this?" answer; nothing matched at all
/// yields an empty string. Matches that exist but do not fit the cap
/// still yield the header and an omission note, never a silent blank.
/// An injected entry always outranks a tail note: a note that cannot fit
/// is dropped, and the caller still sees the omission in `slugs`.
pub fn render_lessons(matched: &Matched) -> (String, Vec<String>) {
    let mut out = String::from(LESSON_HEADER);
    let mut slugs = Vec::new();
    let mut omitted = 0usize;
    for m in matched.lessons.iter().take(LESSON_MAX_ENTRIES) {
        let entry = lesson_entry(m, &matched.label(m));
        if out.len() + entry.len() > LESSON_MAX_BYTES {
            omitted += 1;
            continue;
        }
        out.push_str(&entry);
        slugs.push(m.front.id.clone());
    }
    let extra = matched.lessons.len().saturating_sub(LESSON_MAX_ENTRIES) + omitted;
    if extra > 0 {
        push_note(
            &mut out,
            &format!(
                "\n({extra} more matched omitted at the lesson cap — `cadence memory ls` lists them)\n"
            ),
        );
    }
    if matched.lessons.is_empty() && matched.withheld.is_empty() {
        return (String::new(), vec![]);
    }
    if !matched.withheld.is_empty()
        && out.len() + WITHHELD_HEADER.len() + WITHHELD_NOTE_BUDGET <= LESSON_MAX_BYTES
    {
        out.push_str(WITHHELD_HEADER);
        let mut printed = 0usize;
        for (m, reason) in &matched.withheld {
            // A stored stale mark is free text: redact it before it reaches
            // the dispatch, and never fall back to the raw value.
            let entry = format!("- `{}`: {}\n", m.front.id, redacted_text(reason));
            // Hold back room for the tail note so a cut is always named.
            if out.len() + entry.len() + WITHHELD_NOTE_BUDGET > LESSON_MAX_BYTES {
                break;
            }
            out.push_str(&entry);
            printed += 1;
        }
        let dropped = matched.withheld.len() - printed;
        if dropped > 0 {
            push_note(
                &mut out,
                &format!("({dropped} more withheld — `cadence memory ls` lists them)\n"),
            );
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
    let (review_count, finalization_count, finalized_operations) =
        if let Some(policy) = &m.front.policy {
            let operations: Vec<Value> = policy
                .finalizations
                .iter()
                .rev()
                .take(8)
                .map(|receipt| {
                    let finalizer = match &receipt.finalizer {
                        policy::ActorSnapshot::Agent { identity } => identity.alias.as_str(),
                        policy::ActorSnapshot::Operator { .. } => "operator",
                    };
                    json!({
                        "operation": receipt.operation, "cycle": receipt.cycle,
                        "digest": receipt.digest, "finalized_at": receipt.recorded_at,
                        "finalizer": finalizer,
                    })
                })
                .collect();
            (policy.reviews.len(), policy.finalizations.len(), operations)
        } else {
            let operations = m
                .front
                .finalizations
                .iter()
                .map(|receipt| {
                    json!({
                        "operation": receipt.operation, "cycle": receipt.cycle,
                        "digest": receipt.digest, "finalized_at": receipt.finalized_at,
                        "finalizer": receipt.finalizer.alias,
                    })
                })
                .collect();
            (
                m.front.reviews.len(),
                m.front.finalizations.len(),
                operations,
            )
        };
    let policy_summary = m.front.policy.as_ref().map(|policy| json!({
        "revision": policy.revision,
        "nature": policy.nature,
        "provenance": policy.provenance,
        "audience": policy.audience,
        "roles": policy.roles,
        "author_vendor": policy.author.model.as_ref().and_then(|model| model.vendor.as_deref()),
        "contributors_count": policy.contributors.len(),
        "evidence_refs_count": policy.evidence_refs.len()
            + policy.attestations.iter().map(|item| item.evidence_refs.len()).sum::<usize>(),
        "requirements": policy::requirements(policy).collect::<Vec<_>>(),
        "verification_condition": policy.verification_condition,
        "feedback": {
            "used_reports": policy.feedback.used_reports,
            "wrong_reports": policy.feedback.wrong_reports,
            "reread_reports": policy.feedback.reread_reports,
            "last_used_at": policy.feedback.last_use.as_ref().map(|item| item.recorded_at.as_str()),
        },
    }));
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
        "review_count": review_count,
        "finalization_count": finalization_count,
        "finalized_operations": finalized_operations,
        "policy": policy_summary,
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
    detail_json_with_evidence(m, fresh, None, None)
}

/// Detail JSON plus the read-only evidence analysis. Callers that hold the
/// tracker pass `Some(pm)` so the CLI and the board read surface answer the
/// same way; a failure is reported as an `unknown` result rather than failing
/// the read, and nothing here launches a check.
pub fn detail_json_with_evidence(
    m: &Memory,
    fresh: &Freshness,
    pm: Option<&crate::issue::Pm>,
    caller: Option<&crate::wiki::Caller>,
) -> Value {
    let mut v = card_json(m, fresh);
    v["body"] = json!(m.body);
    if let Some(pm) = pm {
        let caller = caller.unwrap_or(&crate::wiki::Caller::Public);
        v["evidence_analysis"] = match super::evidence::analyse_as(pm, m, caller) {
            Ok(receipt) => receipt.to_json(),
            Err(error) => {
                let safe = |text: &str| {
                    crate::secret::redact_text(text)
                        .unwrap_or_else(|_| "redaction unavailable".to_string())
                };
                let condition = m
                    .front
                    .policy
                    .as_ref()
                    .and_then(|policy| policy.verification_condition.as_ref());
                let mut receipt = super::evidence::Receipt {
                    candidate_digest: semantic_digest(m),
                    check_id: safe(
                        condition
                            .map(|condition| condition.check_id.as_str())
                            .unwrap_or("evidence-analysis"),
                    ),
                    source_revision: condition
                        .and_then(|condition| condition.source_revision.as_deref())
                        .map(safe),
                    target_revision: condition
                        .and_then(|condition| condition.target_revision.as_deref())
                        .map(safe),
                    wiki_target_revision: condition
                        .and_then(|condition| condition.wiki_target_revision.as_deref())
                        .map(safe),
                    outcome: super::evidence::Outcome::Unknown,
                    checked: Vec::new(),
                    skipped: vec![
                        "citation resolution, target applicability, duplicate, contradiction and overfit checks incomplete; see error".to_string(),
                    ],
                    findings: Vec::new(),
                }
                .to_json();
                receipt["error"] = json!(safe(&error.to_string()));
                receipt
            }
        };
    }
    v
}
