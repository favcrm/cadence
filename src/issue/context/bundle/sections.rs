//! Memory and document section builders for `issue::context::bundle`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::HashSet;

use serde_json::{json, Value};

use super::super::{
    BlobError, ManifestDocument, MAX_EXCERPT_BYTES, MAX_MANIFEST_ENTRIES, MAX_MEMORY_FIELD_BYTES,
    MAX_MEMORY_ID_BYTES, MAX_PATH_BYTES, MAX_REASON_BYTES, MAX_TITLE_BYTES,
};
use super::shorten;
use crate::issue::{project, Pm};
use crate::memory;

pub(super) fn memory_context(pm: &Pm, selected: &project::Project, paths: &[String]) -> Value {
    let (pool, load_errors) = memory::load_project_report(&pm.dir, &selected.key);
    let context = memory::MatchCtx {
        components: selected.components.clone(),
        paths: paths.to_vec(),
        providers: Vec::new(),
        tags: Vec::new(),
        roles: Vec::new(),
    };
    let fresh = memory::Freshness::for_project(Some(selected));
    let matched = memory::match_memories(&pool, &context, &fresh);
    let (lessons, slugs) = memory::render_lessons(&matched);
    let included_ids: HashSet<&str> = slugs.iter().map(String::as_str).collect();
    let included: Vec<Value> = matched
        .lessons
        .iter()
        .filter(|memory| included_ids.contains(memory.front.id.as_str()))
        .map(|memory| {
            json!({
                "id": shorten(&memory.front.id, MAX_MEMORY_ID_BYTES).0,
                "kind": shorten(&memory.front.kind, MAX_MEMORY_FIELD_BYTES).0,
                "confidence": shorten(&memory.front.confidence, MAX_MEMORY_FIELD_BYTES).0,
                "verified_at": memory::last_verified(memory)
                    .map(|at| shorten(at, MAX_MEMORY_FIELD_BYTES).0),
                "evidence": shorten(&matched.label(memory), MAX_MEMORY_ID_BYTES).0,
            })
        })
        .collect();
    let matched_ids: HashSet<&str> = matched
        .lessons
        .iter()
        .map(|memory| memory.front.id.as_str())
        .collect();
    let stale: std::collections::HashMap<&str, &str> = matched
        .withheld
        .iter()
        .map(|(memory, reason)| (memory.front.id.as_str(), reason.as_str()))
        .collect();
    let withheld_all: Vec<Value> = pool
        .iter()
        .filter(|memory| !included_ids.contains(memory.front.id.as_str()))
        .map(|memory| {
            let reason = if matched_ids.contains(memory.front.id.as_str()) {
                "lesson renderer omitted this eligible entry at its cap".to_string()
            } else if let Some(reason) = stale.get(memory.front.id.as_str()) {
                reason.to_string()
            } else if memory.front.status == "accepted" {
                let (eligible, reason) = memory::retrieval_status(memory);
                if !eligible {
                    reason
                } else {
                    "not applicable to the selected context".to_string()
                }
            } else {
                format!(
                    "review blocked: memory status is {}",
                    shorten(&memory.front.status, MAX_MEMORY_FIELD_BYTES).0
                )
            };
            json!({
                "id": shorten(&memory.front.id, MAX_MEMORY_ID_BYTES).0,
                "status": shorten(&memory.front.status, MAX_MEMORY_FIELD_BYTES).0,
                "reason": shorten(&reason, 256).0,
            })
        })
        .collect();
    let withheld_total = withheld_all.len();
    let withheld = withheld_all
        .into_iter()
        .take(MAX_MANIFEST_ENTRIES)
        .collect::<Vec<_>>();
    let withheld_omitted = withheld_total.saturating_sub(withheld.len());
    let load_errors_total = load_errors.len();
    let errors: Vec<String> = load_errors
        .into_iter()
        .take(MAX_MANIFEST_ENTRIES)
        .map(|error| shorten(&error, 256).0)
        .collect();
    let load_errors_omitted = load_errors_total.saturating_sub(errors.len());
    json!({
        "included": included,
        "lessons": lessons,
        "withheld": withheld,
        "withheld_total": withheld_total,
        "withheld_omitted": withheld_omitted,
        "load_errors": errors,
        "load_errors_total": load_errors_total,
        "load_errors_omitted": load_errors_omitted,
        "matched_total": matched.lessons.len(),
    })
}

pub(super) fn document_value(
    document: &ManifestDocument,
    selected: bool,
    selection_reason: &str,
    result: Result<Option<String>, BlobError>,
) -> (Value, bool, Option<String>) {
    let id = shorten(&document.id, 48).0;
    let kind = shorten(&document.kind, 32).0;
    let path = shorten(&document.path, MAX_PATH_BYTES).0;
    let title = shorten(&document.title, MAX_TITLE_BYTES).0;
    if !selected {
        if let Err(error) = result {
            return (
                json!({
                    "id": id,
                    "kind": kind,
                    "path": path,
                    "title": title,
                    "required": document.required,
                    "selected": false,
                    "selection_reason": selection_reason,
                    "state": error.state(),
                    "reason": shorten(&error.reason(), MAX_REASON_BYTES).0,
                }),
                false,
                None,
            );
        }
        return (
            json!({
                "id": id,
                "kind": kind,
                "path": path,
                "title": title,
                "required": document.required,
                "selected": false,
                "selection_reason": selection_reason,
                "state": "not_selected",
            }),
            false,
            None,
        );
    }
    match result {
        Ok(Some(text)) => {
            let (excerpt, truncated) = shorten(&text, MAX_EXCERPT_BYTES);
            (
                json!({
                    "id": id,
                    "kind": kind,
                    "path": path,
                    "title": title,
                    "required": document.required,
                    "selected": true,
                    "selection_reason": selection_reason,
                    "state": "ready",
                    "bytes": text.len(),
                    "excerpt": excerpt,
                    "truncated": truncated,
                }),
                true,
                Some(document.path.clone()),
            )
        }
        Ok(None) => (
            json!({
                "id": id,
                "kind": kind,
                "path": path,
                "title": title,
                "required": document.required,
                "selected": true,
                "selection_reason": selection_reason,
                "state": "missing",
                "reason": "tracked blob is absent at HEAD",
            }),
            false,
            None,
        ),
        Err(error) => (
            json!({
                "id": id,
                "kind": kind,
                "path": path,
                "title": title,
                "required": document.required,
                "selected": true,
                "selection_reason": selection_reason,
                "state": error.state(),
                "reason": shorten(&error.reason(), MAX_REASON_BYTES).0,
            }),
            false,
            None,
        ),
    }
}
