//! Member visibility: every rule for what a non-operator may see of the board's
//! agent rows, running items, events, thread pages and master state.

use serde_json::{json, Value};

/// CAD-1767: a running message as a non-operator may see it - ONE
/// allowlist of non-content fields (id, state, task id, created), applied
/// to a raw daemon message (`task_id`) and to an already-reduced item
/// (`task`) alike. No `summary` and nothing else, so a field added to the
/// message later is dropped by default.
pub(crate) fn member_running_item(item: &Value) -> Value {
    json!({
        "id": item["id"],
        "state": "running",
        "task": if item["task"].is_null() { &item["task_id"] } else { &item["task"] },
        "created": item["created"],
    })
}

/// The fleet-row fields a non-operator keeps: identity, runtime state,
/// counts, model and effort facts, ids and timing - what the roster, the
/// cards and the agent page read. Any field not named here is dropped, so a
/// field added to the row later stays hidden from members by default.
const MEMBER_ROW_FIELDS: &[&str] = &[
    "alias",
    "provider",
    "endpoint_kind",
    "role",
    "team_role",
    "catalogue",
    "look",
    "kind",
    "model_selection",
    "model_lookup_role",
    "model",
    "model_reported",
    "model_configured",
    "model_source",
    "effort",
    "effort_reported",
    "effort_source",
    "effort_applicable",
    "quota",
    "usage_limit",
    "state",
    "group",
    "group_root",
    "running",
    "queued",
    "unread",
    "oldest_unread_age_secs",
    "unknown",
    "parked",
    "fenced",
    "on",
    "recovery",
    "resume",
    "resume_hint",
    "dead",
    "inbox",
    "last_activity",
    "silent_secs",
    "stalled",
    "state_label",
    "event_cursor",
];

/// A bound task as a non-operator sees it: ids, states and titles, with
/// its running kickoff reduced to the one running-item allowlist.
const MEMBER_TASK_FIELDS: &[&str] = &[
    "task",
    "task_state",
    "title",
    "issue",
    "job",
    "job_title",
    "job_state",
];

fn member_message(item: &Value) -> Value {
    if item.is_object() {
        member_running_item(item)
    } else {
        Value::Null
    }
}

/// CAD-1767: one fleet row rebuilt for a non-operator from the allowlists
/// above - never patched from the full row.
pub(crate) fn member_agent_row(row: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in MEMBER_ROW_FIELDS {
        if let Some(value) = row.get(*key) {
            out.insert((*key).to_string(), value.clone());
        }
    }
    out.insert("message".to_string(), member_message(&row["message"]));
    if let Some(items) = row["running_messages"].as_array() {
        out.insert(
            "running_messages".to_string(),
            Value::Array(items.iter().map(member_running_item).collect()),
        );
    }
    if let Some(tasks) = row["tasks"].as_array() {
        let rebuilt: Vec<Value> = tasks
            .iter()
            .map(|task| {
                let mut t = serde_json::Map::new();
                for key in MEMBER_TASK_FIELDS {
                    if let Some(value) = task.get(*key) {
                        t.insert((*key).to_string(), value.clone());
                    }
                }
                t.insert("message".to_string(), member_message(&task["message"]));
                Value::Object(t)
            })
            .collect();
        out.insert("tasks".to_string(), Value::Array(rebuilt));
    }
    Value::Object(out)
}

/// The fleet payload for a non-operator: every row rebuilt.
pub(crate) fn redact_agents_payload(payload: &mut Value) {
    if let Some(rows) = payload["agents"].as_array_mut() {
        for row in rows.iter_mut() {
            *row = member_agent_row(row);
        }
    }
}

/// CAD-1767: what a non-operator may see of one event - an ALLOWLIST, so
/// anything a payload gains later is dropped by default. Top level: the
/// ids, kind and time. Payload: only these scalar fields; every other one
/// (`result`, `text`, `body`, `before`/`after`, …) is dropped.
const MEMBER_EVENT_FIELDS: &[&str] = &["seq", "alias", "kind", "job_id", "task_id", "at"];
const MEMBER_EVENT_PAYLOAD: &[&str] = &[
    "message",
    "turn_id",
    "state",
    "status",
    "disposition",
    "reason",
    "error",
];

/// A short machine code (`snake_case`, no spaces): the only form of
/// `error` / `reason` a non-operator keeps. Free text - a provider's own
/// message, a sentence - is dropped, because it can quote message content.
fn is_machine_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b':' | b'-')
        })
}

pub(crate) fn member_event(event: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in MEMBER_EVENT_FIELDS {
        if let Some(value) = event.get(*key) {
            out.insert((*key).to_string(), value.clone());
        }
    }
    let mut payload = serde_json::Map::new();
    if let Some(fields) = event["payload"].as_object() {
        for key in MEMBER_EVENT_PAYLOAD {
            match fields.get(*key) {
                Some(value) if value.is_object() || value.is_array() => {}
                Some(Value::String(text))
                    if matches!(*key, "error" | "reason") && !is_machine_code(text) => {}
                Some(value) => {
                    payload.insert((*key).to_string(), value.clone());
                }
                None => {}
            }
        }
    }
    out.insert("payload".to_string(), Value::Object(payload));
    Value::Object(out)
}

/// CAD-1767: what a non-operator reader of a thread page may see. Members
/// read agent threads, but never the operator-only entries
/// (`store::operator_only_kind`: artifact cards, hire proposals) and never
/// the master's Home conversation. Fail closed: only a proven operator
/// gets the page whole. Whether this is the master's Home is decided from the
/// DAEMON's reply (the resolved `alias` and the thread's `install_id`), never
/// from the URL's spelling: an absent field counts as Home. It edits `page`
/// in place; the stream collects the page's seqs itself so its cursor still
/// advances past what was hidden.
pub(super) fn member_view(page: &mut Value) {
    let resolved_master = page["alias"].as_str().is_none_or(crate::master::is_master);
    let home = resolved_master
        && page["thread"].as_object().is_none_or(|t| {
            t.get("install_id")
                .is_none_or(|i| i.is_null() || i.as_str() == Some(crate::store::RECRUIT_INSTALL))
        });
    if let Some(entries) = page["entries"].as_array_mut() {
        entries.retain(|e| {
            !home
                && !e["kind"]
                    .as_str()
                    .is_some_and(crate::store::operator_only_kind)
        });
    }
    if home {
        page["thread"] = Value::Null;
    }
}

/// CAD-1767: what a non-operator may see of the master's state: the state,
/// queue counts and routing metadata, but never the text of the operator's
/// turn. The turn is REBUILT from an allowlist, so a field added later is
/// dropped by default.
pub(super) fn redact_master_state(state: &mut Value) {
    const TURN_FIELDS: &[&str] = &[
        "state",
        "message",
        "conversation",
        "app_origin",
        "since",
        "position",
    ];
    if let Some(turn) = state.get("turn").and_then(Value::as_object) {
        let kept: serde_json::Map<String, Value> = TURN_FIELDS
            .iter()
            .filter_map(|k| turn.get(*k).map(|v| ((*k).to_string(), v.clone())))
            .collect();
        state["turn"] = Value::Object(kept);
    }
}
