//! Context-bundle assembly, memory matching, and fail-closed response capping.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::HashSet;

use serde_json::{json, Value};

use super::{
    path_error, preferred_manifest_path, read_blob, resolve_source, select_manifest,
    validate_manifest, BlobError, ContextManifest, ManifestDocument, RepoSnapshot,
    DEFAULT_MANIFEST_PATH, MAX_DOCUMENT_BYTES, MAX_EXCERPT_BYTES, MAX_GIT_OUTPUT_BYTES,
    MAX_MANIFEST_BYTES, MAX_MANIFEST_ENTRIES, MAX_MEMORY_FIELD_BYTES, MAX_MEMORY_ID_BYTES,
    MAX_PATH_BYTES, MAX_REASON_BYTES, MAX_RESPONSE_BYTES, MAX_TITLE_BYTES,
};
use crate::issue::{project, Pm};
use crate::memory;

pub(super) fn shorten(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

fn memory_context(pm: &Pm, selected: &project::Project, paths: &[String]) -> Value {
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

fn document_value(
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

/// Bound the serialized response to [`MAX_RESPONSE_BYTES`] by halving excerpts,
/// widest first, until it fits. Document identity, path, state, size and
/// requiredness are never dropped; every shortened excerpt is flagged
/// `truncated` and the reduction count is reported in `limits`.
///
/// Excerpts are not the only untrusted content: withheld memory metadata and
/// Git diagnostics are bounded where they are copied, and this final check
/// still fails closed when the payload cannot fit without excerpts — optional
/// sections are dropped with their counts kept, and the last resort is a
/// `too_large` state instead of oversized bytes.
pub(super) fn cap_response(mut payload: Value) -> Value {
    let mut reductions = 0usize;
    let mut dropped = false;
    // Withheld memory metadata is untrusted and can alone exceed the bound.
    // Drop those entries first, so the document excerpts survive a payload
    // that only memory metadata made too large.
    if bounded_len(&payload) > MAX_RESPONSE_BYTES {
        let withheld_bytes = payload
            .pointer("/memories/withheld")
            .map(bounded_len)
            .unwrap_or(0);
        if withheld_bytes > 2
            && bounded_len(&payload).saturating_sub(withheld_bytes - 2) <= MAX_RESPONSE_BYTES
        {
            drop_memory_entries(&mut payload);
            dropped = true;
        }
    }
    loop {
        if bounded_len(&payload) <= MAX_RESPONSE_BYTES {
            break;
        }
        let mut changed = false;
        if let Some(documents) = payload.get_mut("documents").and_then(Value::as_array_mut) {
            // Reduce the widest excerpt first. Taking them in array order
            // drains the last document to an empty excerpt while every
            // earlier document keeps its full text; the same byte budget can
            // instead keep a bounded excerpt of every document.
            let mut widest: Option<(usize, usize)> = None;
            for (index, document) in documents.iter().enumerate() {
                let Some(excerpt) = document.get("excerpt").and_then(Value::as_str) else {
                    continue;
                };
                if excerpt.is_empty() {
                    continue;
                }
                if widest.is_none_or(|(_, best)| excerpt.len() > best) {
                    widest = Some((index, excerpt.len()));
                }
            }
            if let Some((index, len)) = widest {
                let (shortened, _) =
                    shorten(documents[index]["excerpt"].as_str().unwrap_or(""), len / 2);
                documents[index]["excerpt"] = Value::String(shortened);
                documents[index]["truncated"] = Value::Bool(true);
                changed = true;
                reductions += 1;
            }
        }
        if !changed {
            break;
        }
    }
    if let Some(limits) = payload.get_mut("limits") {
        limits["response_truncated"] = Value::Bool(reductions > 0 || dropped);
        limits["response_excerpt_reductions"] = Value::from(reductions);
    }
    // The bookkeeping above adds bytes of its own, so the bound is judged on
    // the final payload, not on the loop's last measurement.
    if bounded_len(&payload) <= MAX_RESPONSE_BYTES {
        return payload;
    }
    // Non-excerpt content alone exceeds the bound. Memory entries are
    // evidence, not the document contract: keep every count, drop the entries.
    drop_memory_entries(&mut payload);
    if bounded_len(&payload) <= MAX_RESPONSE_BYTES {
        return mark_truncated(payload);
    }
    // Documents keep their identity and state; their bodies are dropped.
    if let Some(documents) = payload.get_mut("documents").and_then(Value::as_array_mut) {
        for document in documents.iter_mut() {
            if document.get("excerpt").is_some() {
                document["excerpt"] = Value::String(String::new());
                document["truncated"] = Value::Bool(true);
            }
        }
    }
    if bounded_len(&payload) <= MAX_RESPONSE_BYTES {
        return mark_truncated(payload);
    }
    // Last resort: the payload cannot carry its own metadata within the
    // bound. Report the state and the counts, never the oversized bytes.
    fail_closed(payload)
}

pub(super) fn bounded_len(payload: &Value) -> usize {
    serde_json::to_vec_pretty(payload)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

fn mark_truncated(mut payload: Value) -> Value {
    if let Some(limits) = payload.get_mut("limits") {
        limits["response_truncated"] = Value::Bool(true);
    }
    payload
}

/// Drop every memory entry, keeping the totals and the omitted counts that
/// explain the gap. The lesson text goes too: it repeats entries.
fn drop_memory_entries(payload: &mut Value) {
    let Some(memories) = payload.get_mut("memories").and_then(Value::as_object_mut) else {
        return;
    };
    let withheld_total = memories
        .get("withheld_total")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let load_errors_total = memories
        .get("load_errors_total")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    memories.insert("included".to_string(), Value::Array(Vec::new()));
    memories.insert("lessons".to_string(), Value::String(String::new()));
    memories.insert("withheld".to_string(), Value::Array(Vec::new()));
    memories.insert("withheld_omitted".to_string(), Value::from(withheld_total));
    memories.insert("load_errors".to_string(), Value::Array(Vec::new()));
    memories.insert(
        "load_errors_omitted".to_string(),
        Value::from(load_errors_total),
    );
}

/// The bounded payload cannot fit even with every optional section dropped.
/// Return the state and counts a caller needs to explain the gap, and no
/// oversized bytes.
fn fail_closed(payload: Value) -> Value {
    let memories = payload.get("memories");
    let count = |key: &str| {
        memories
            .and_then(|memories| memories.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let documents_omitted = payload
        .get("documents")
        .and_then(Value::as_array)
        .map(|documents| documents.len())
        .unwrap_or(0);
    let reductions = payload
        .pointer("/limits/response_excerpt_reductions")
        .cloned()
        .unwrap_or(Value::from(0));
    let mut limits = limits_json(true);
    limits["response_excerpt_reductions"] = reductions;
    let manifest_path = payload
        .pointer("/manifest/path")
        .cloned()
        .unwrap_or_else(|| Value::from(DEFAULT_MANIFEST_PATH));
    json!({
        "project": payload.get("project").cloned().unwrap_or(Value::Null),
        "state": "too_large",
        "manifest": {
            "path": manifest_path,
            "state": "too_large",
            "revision": payload.pointer("/manifest/revision").cloned().unwrap_or(Value::Null),
            "entry_count": payload.pointer("/manifest/entry_count").cloned().unwrap_or(Value::Null),
            "documents_omitted": documents_omitted,
            "errors": ["context metadata exceeds the response bound; document bodies and memory entries are omitted"],
        },
        "snapshot": payload.get("snapshot").cloned().unwrap_or(Value::Null),
        "documents": [],
        "memories": {
            "included": [],
            "lessons": "",
            "withheld": [],
            "withheld_total": count("withheld_total"),
            "withheld_omitted": count("withheld_total"),
            "load_errors": [],
            "load_errors_total": count("load_errors_total"),
            "load_errors_omitted": count("load_errors_total"),
            "matched_total": count("matched_total"),
        },
        "limits": limits,
    })
}

/// Build a context response. This never returns a filesystem error to callers:
/// project/source/document failures are stateful JSON so the UI can explain
/// missing or blocked context without guessing.
pub fn bundle(
    pm: &Pm,
    selected: &project::Project,
    role: Option<&str>,
    expected_revision: Option<&str>,
) -> Value {
    let source = match resolve_source(selected, expected_revision) {
        Ok(source) => source,
        Err(error) => {
            let reason = shorten(&error.message, MAX_REASON_BYTES).0;
            return json!({
                "project": selected.key,
                "state": error.state,
                "manifest": {"path": preferred_manifest_path(&selected.key), "state": error.state, "errors": [reason.clone()]},
                "snapshot": RepoSnapshot {
                    head_revision: None,
                    expected_revision: expected_revision.map(str::to_string),
                    revision_state: "unknown".to_string(),
                    dirty: None,
                    dirty_truncated: false,
                    repo_identity: None,
                    error: Some(reason),
                },
                "documents": [],
                "memories": empty_memories(),
                "limits": limits_json(false),
            });
        }
    };
    let (manifest_path, manifest_result) =
        select_manifest(&source.root, &source.head, &selected.key);
    let manifest_text = match manifest_result {
        Ok(Some(text)) => text,
        Ok(None) => {
            return cap_response(json!({
                "project": selected.key,
                "state": "missing",
                "manifest": {"path": manifest_path, "state": "missing", "revision": source.head, "errors": ["tracked manifest is absent at HEAD"]},
                "snapshot": source.snapshot,
                "documents": [],
                "memories": empty_memories(),
                "limits": limits_json(false),
            }));
        }
        Err(error) => {
            let state = error.state();
            let reason = shorten(&error.reason(), MAX_REASON_BYTES).0;
            return cap_response(json!({
                "project": selected.key,
                "state": state,
                "manifest": {"path": manifest_path, "state": state, "revision": source.head, "errors": [reason]},
                "snapshot": source.snapshot,
                "documents": [],
                "memories": empty_memories(),
                "limits": limits_json(false),
            }));
        }
    };
    if manifest_text.len() > MAX_MANIFEST_BYTES {
        return cap_response(json!({
            "project": selected.key,
            "state": "too_large",
            "manifest": {"path": manifest_path, "state": "too_large", "revision": source.head, "errors": [format!("manifest exceeds {MAX_MANIFEST_BYTES} bytes")]},
            "snapshot": source.snapshot,
            "documents": [],
            "memories": empty_memories(),
            "limits": limits_json(false),
        }));
    }
    let manifest: ContextManifest = match serde_yaml::from_str(&manifest_text) {
        Ok(manifest) => manifest,
        Err(error) => {
            return cap_response(json!({
                "project": selected.key,
                "state": "conflict",
                "manifest": {"path": manifest_path, "state": "conflict", "revision": source.head, "errors": [shorten(&format!("manifest YAML is invalid: {error}"), 512).0]},
                "snapshot": source.snapshot,
                "documents": [],
                "memories": empty_memories(),
                "limits": limits_json(false),
            }));
        }
    };
    let manifest_errors = validate_manifest(&manifest, &selected.key);
    let valid_manifest = manifest_errors.is_empty();
    let manifest_entry_count = manifest.documents.len();
    let manifest_entries_omitted = manifest_entry_count.saturating_sub(MAX_MANIFEST_ENTRIES);
    let manifest_errors = manifest_errors
        .into_iter()
        .map(|error| shorten(&error, 512).0)
        .collect::<Vec<_>>();
    let mut documents = Vec::new();
    let mut selected_paths = Vec::new();
    let mut required_failure = false;
    for document in manifest.documents.iter().take(MAX_MANIFEST_ENTRIES) {
        let role_selected = role
            .map(|role| {
                document.roles.is_empty()
                    || document.required
                    || document.roles.iter().any(|r| r == role)
            })
            .unwrap_or(true);
        let is_selected = valid_manifest && role_selected;
        let reason = if document.required {
            "required".to_string()
        } else if let Some(role) = role {
            if document.roles.is_empty() {
                "optional-default".to_string()
            } else if document.roles.iter().any(|candidate| candidate == role) {
                format!("role:{role}")
            } else {
                "excluded:role-mismatch".to_string()
            }
        } else {
            "optional-default".to_string()
        };
        let read = if let Some(reason) = path_error(&document.path) {
            Err(BlobError::State("invalid_path", reason))
        } else if is_selected {
            read_blob(&source.root, &source.head, &document.path)
        } else {
            Ok(None)
        };
        let (value, ready, path) = document_value(document, is_selected, &reason, read);
        if document.required && (!is_selected || !ready) {
            required_failure = true;
        }
        if let Some(path) = path {
            selected_paths.push(path);
        }
        if is_selected
            && path_error(&document.path).is_none()
            && !selected_paths.iter().any(|path| path == &document.path)
        {
            // A manifest path is context even when its tracked blob is
            // missing or unreadable; memory matching must not depend on a
            // document happening to be available at this HEAD.
            selected_paths.push(document.path.clone());
        }
        documents.push(value);
    }
    let state = if !valid_manifest {
        "conflict"
    } else if required_failure {
        let required_state = |wanted: &str| {
            documents.iter().any(|document| {
                document["required"].as_bool() == Some(true) && document["state"] == wanted
            })
        };
        if required_state("missing") {
            "missing"
        } else if required_state("too_large") {
            "too_large"
        } else if required_state("unreadable") {
            "unreadable"
        } else if required_state("invalid_path") {
            "invalid_path"
        } else {
            "blocked"
        }
    } else {
        "ready"
    };
    let memories = if valid_manifest {
        memory_context(pm, selected, &selected_paths)
    } else {
        empty_memories()
    };
    cap_response(json!({
        "project": selected.key,
        "state": state,
        "manifest": {
            "path": manifest_path,
            "state": if valid_manifest {"ready"} else {"conflict"},
            "revision": source.head,
            "entry_count": manifest_entry_count,
            "entries_omitted": manifest_entries_omitted,
            "errors": manifest_errors,
        },
        "snapshot": source.snapshot,
        "documents": documents,
        "memories": memories,
        "limits": limits_json(false),
    }))
}

pub(super) fn limits_json(response_truncated: bool) -> Value {
    json!({
        "max_manifest_bytes": MAX_MANIFEST_BYTES,
        "max_manifest_entries": MAX_MANIFEST_ENTRIES,
        "max_path_bytes": MAX_PATH_BYTES,
        "max_title_bytes": MAX_TITLE_BYTES,
        "max_document_bytes": MAX_DOCUMENT_BYTES,
        "max_excerpt_bytes": MAX_EXCERPT_BYTES,
        "max_git_output_bytes": MAX_GIT_OUTPUT_BYTES,
        "max_memory_entries": memory::LESSON_MAX_ENTRIES,
        "max_memory_lesson_bytes": memory::LESSON_MAX_BYTES,
        "max_response_bytes": MAX_RESPONSE_BYTES,
        "response_truncated": response_truncated,
        "response_excerpt_reductions": 0,
    })
}

pub(super) fn empty_memories() -> Value {
    json!({
        "included": [],
        "lessons": "",
        "withheld": [],
        "withheld_total": 0,
        "withheld_omitted": 0,
        "load_errors": [],
        "load_errors_total": 0,
        "load_errors_omitted": 0,
        "matched_total": 0,
    })
}
