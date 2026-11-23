//! Readiness evaluation and caller-facing read projection for `crm-campaign-work/v1`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde_json::{json, Map, Value};

use super::{ContentFacts, WorkRecord, WorkStatus, CONTRACT, UNRECORDED};

/// What the CRM readiness chain reports, as four recorded facts. These
/// are the inputs to `next_decision`; the host never derives them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadinessFacts {
    pub draft: Option<bool>,
    pub audience_valid: Option<bool>,
    pub sender_live: Option<bool>,
    pub test_accepted: Option<bool>,
}

impl ReadinessFacts {
    fn complete(&self) -> bool {
        self.draft.is_some()
            && self.audience_valid.is_some()
            && self.sender_live.is_some()
            && self.test_accepted.is_some()
    }
}

/// The one next-decision algorithm, applied only when the caller could read
/// the readiness chain (`read` gates on that). The recorded lifecycle wins
/// first; then the readiness answers decide in task order. A readiness answer
/// that is not recorded yields `unknown` rather than a failed check.
pub fn next_decision(work: Option<&WorkRecord>, facts: &ReadinessFacts) -> Value {
    if let Some(row) = work {
        if matches!(row.status, Some(WorkStatus::Done | WorkStatus::Cancelled)) {
            return json!({
                "key": "closed",
                "label": "Closed — no further work",
                "reason": Value::Null,
            });
        }
        if row.status == Some(WorkStatus::Blocked) || row.blocked_reason.is_some() {
            let label = match &row.blocked_reason {
                None => "Unblock this campaign".to_string(),
                Some(reason) => format!("Unblock: {reason}"),
            };
            return json!({
                "key": "unblock",
                "label": label,
                "reason": row.blocked_reason,
            });
        }
    }
    if !facts.complete() {
        return json!({
            "key": "unknown",
            "label": "Campaign readiness is not recorded",
            "reason": Value::Null,
        });
    }
    let (key, label) = if facts.draft == Some(false) {
        ("draft", "Draft the campaign email")
    } else if facts.audience_valid == Some(false) {
        ("audience", "Freeze and recheck the audience")
    } else if facts.sender_live == Some(false) {
        ("sender", "Connect a live email sender")
    } else if facts.test_accepted == Some(false) {
        ("test", "Send an accepted test email")
    } else {
        ("send", "Prepare and send the campaign")
    };
    json!({"key": key, "label": label, "reason": Value::Null})
}

/// The recorded facts body `campaign-work.show` answers with, and the
/// body `campaign-work.save` answers after its write. Closed keys: the
/// app refuses a reply that grew an authority-shaped field.
pub fn facts_json(
    app: &str,
    install: &str,
    context: Option<&str>,
    campaign: &str,
    work: Option<&WorkRecord>,
    content: Option<&ContentFacts>,
) -> Value {
    json!({
        "contract": CONTRACT,
        "source": {"app": app, "install_id": install, "context_id": context},
        "campaign_id": campaign,
        "work": work.map(WorkRecord::to_json),
        "content": content.map(|content| json!({
            "revision": content.revision,
            "digest": content.digest,
        })),
    })
}

/// Whether the caller is authorized for this source. The caller proves
/// it before reading; the adapter never decides it from request text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Authorized,
    Forbidden,
}

/// One read for an authorized caller. `readiness` is `None` when the
/// caller cannot read the CRM readiness chain — the read is then
/// `partial` (or `unavailable`), never a guess.
pub struct ReadInput<'a> {
    pub app: &'a str,
    pub install: &'a str,
    pub context: Option<&'a str>,
    pub campaign: &'a str,
    pub work: Option<&'a WorkRecord>,
    pub content: Option<&'a ContentFacts>,
    pub readiness: Option<&'a ReadinessFacts>,
    pub access: Access,
    /// The caller's last-read work revision; a mismatch refuses values.
    pub expected_revision: Option<i64>,
}

/// `app:install:context:campaign` — the source-qualified identity. Two
/// installations may hold the same campaign id; this key never collides.
pub fn identity(app: &str, install: &str, context: Option<&str>, campaign: &str) -> String {
    format!("{app}:{install}:{}:{campaign}", context.unwrap_or("-"))
}

/// The canonical link. It is only an address: it names nothing without
/// the source identity beside it.
pub fn link(app: &str, campaign: &str) -> String {
    format!("{app}:campaigns/{campaign}")
}

/// Project recorded facts into the read contract. State order is
/// forbidden → stale → unavailable/partial/ok: only an authorized,
/// revision-current read returns recorded values.
pub fn read(input: ReadInput<'_>) -> Value {
    let mut reply = Map::new();
    reply.insert("contract".into(), json!(CONTRACT));
    reply.insert("state".into(), Value::Null);
    reply.insert(
        "source".into(),
        json!({
            "app": input.app,
            "install_id": input.install,
            "context_id": input.context,
        }),
    );
    reply.insert(
        "identity".into(),
        json!(identity(
            input.app,
            input.install,
            input.context,
            input.campaign
        )),
    );
    reply.insert(
        "campaign".into(),
        json!({"id": input.campaign, "link": link(input.app, input.campaign)}),
    );
    // Every refused read carries the same closed shape with null values.
    for key in [
        "revision",
        "content_revision",
        "content",
        "assignment",
        "due_at",
        "lifecycle",
        "next_decision",
    ] {
        reply.insert(key.into(), Value::Null);
    }
    reply.insert("unavailable".into(), json!([]));
    reply.insert("refusal".into(), Value::Null);

    if input.access != Access::Authorized {
        let mut unavailable: Vec<&str> = UNRECORDED.to_vec();
        unavailable.push("next_decision");
        put(&mut reply, "state", json!("forbidden"));
        put(&mut reply, "unavailable", json!(unavailable));
        put(
            &mut reply,
            "refusal",
            json!({
                "code": "forbidden",
                "text": "This caller is not authorized for this campaign work source.",
            }),
        );
        return Value::Object(reply);
    }

    if let Some(expected) = input.expected_revision {
        if expected != WorkRecord::cas_revision(input.work) {
            // Revision tokens only: enough to retry against the current
            // row, with no content facts and no recorded values.
            let mut unavailable: Vec<&str> = UNRECORDED.to_vec();
            unavailable.push("next_decision");
            put(&mut reply, "state", json!("stale"));
            put(
                &mut reply,
                "revision",
                json!(WorkRecord::cas_revision(input.work)),
            );
            put(
                &mut reply,
                "content_revision",
                json!(input.work.and_then(|row| row.content_revision)),
            );
            put(&mut reply, "unavailable", json!(unavailable));
            put(
                &mut reply,
                "refusal",
                json!({
                    "code": "stale_revision",
                    "text": "These campaign work details changed. Re-read before using them.",
                }),
            );
            return Value::Object(reply);
        }
    }

    // The app gates the decision itself on readiness: an unreadable chain
    // yields no decision (partial), never a lifecycle shortcut.
    let decision = input
        .readiness
        .map(|facts| next_decision(input.work, facts));
    let has_decision = decision.is_some();
    let mut unavailable: Vec<&str> = Vec::new();
    if !has_decision {
        unavailable.push("next_decision");
    }
    put(&mut reply, "content", content_json(input.content));
    put(&mut reply, "next_decision", decision.unwrap_or(Value::Null));
    let Some(work) = input.work else {
        // Nothing recorded: no revision, no lifecycle. Content facts and
        // a decision from the campaign's own readiness still stand.
        unavailable.extend(UNRECORDED);
        put(&mut reply, "state", json!("unavailable"));
        put(
            &mut reply,
            "content_revision",
            json!(input.content.map(|content| content.revision)),
        );
        put(&mut reply, "unavailable", json!(unavailable));
        return Value::Object(reply);
    };
    if work.assignee.is_none() {
        unavailable.push("assignee");
    }
    if work.assigned_by.is_none() {
        unavailable.push("assigned_by");
    }
    if work.assigned_at.is_none() {
        unavailable.push("assigned_at");
    }
    if work.due_at.is_none() {
        unavailable.push("due_at");
    }
    if work.status.is_none() {
        unavailable.push("status");
    }
    if work.status == Some(WorkStatus::Blocked) && work.blocked_reason.is_none() {
        unavailable.push("blocked_reason");
    }
    put(
        &mut reply,
        "state",
        json!(if has_decision { "ok" } else { "partial" }),
    );
    put(&mut reply, "revision", json!(work.revision));
    put(&mut reply, "content_revision", json!(work.content_revision));
    put(
        &mut reply,
        "assignment",
        json!({
            "assignee": work.assignee,
            "assigned_by": work.assigned_by,
            "assigned_at": work.assigned_at,
        }),
    );
    put(&mut reply, "due_at", json!(work.due_at));
    put(
        &mut reply,
        "lifecycle",
        json!({
            "status": work.status.map(WorkStatus::as_str),
            "blocked_reason": work.blocked_reason,
            "updated_at": work.updated_at,
        }),
    );
    put(&mut reply, "unavailable", json!(unavailable));
    Value::Object(reply)
}

fn put(reply: &mut Map<String, Value>, key: &str, value: Value) {
    reply.insert(key.to_string(), value);
}

fn content_json(content: Option<&ContentFacts>) -> Value {
    content.map_or(
        Value::Null,
        |content| json!({"revision": content.revision, "digest": content.digest}),
    )
}
