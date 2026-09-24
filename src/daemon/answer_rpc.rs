//! An answer reaches the worker who asked (CAD-447).
//!
//! `answer_route {issue, report}` names an `answer` report that was just
//! filed — by the board's answer route or `cadence report file --kind
//! answer` — and queues ONE message to the author of the question it
//! answers, with the answer text and the report's path.
//!
//! Filing stays where it was, and so does who may file; this verb only
//! decides whether the filed answer is genuine enough to send:
//!
//! - the caller comes from the connection alone (the proven operator,
//!   or the agent whose process tree it is) and must be the answer's
//!   recorded author — a file that claims `agent: operator` but is
//!   routed by an agent, a detached child with no identity, or a request
//!   carrying any field but `issue` and `report` sends nothing;
//! - the answer must be a valid stored report naming a question on the
//!   same ticket, and the question's FIRST answer — a later answer to a
//!   question already answered is filed but routes nothing, so no
//!   answerer (the master included) gets a free-text channel to a past
//!   asker; the message goes only to that question's recorded author;
//! - the message id is the daemon's own `sys-answer-<hash>` of the
//!   answer's path, so a retried answer (the tracker returns the same
//!   file for identical content) or a concurrent second call queues
//!   nothing new. Only an identical message — to the asker, source
//!   `answer`, same text, no `reply_to` — counts as that duplicate; any
//!   other holder of the id (a squat) is recorded `answer_undeliverable`
//!   and refused as an error, never a silent duplicate. Reserving `sys-` against callers is CAD-445's (#250);
//! - the message owes nobody a report: it is queued with no `reply_to`,
//!   so it never routes a result to the asker's PM;
//! - an asker that is no longer registered is recorded once per answer
//!   as `answer_undeliverable` on the daemon stream; the answer itself
//!   stands.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{required_str, Shared, DAEMON_ALIAS};
use crate::error::{Error, Result};
use crate::issue::{task_report, write, Pm};
use crate::peer::AgentCaller;
use crate::store;

/// Answer bytes a message carries to a pty pane (one pasted line under
/// the pty ceiling); the report file keeps the rest.
const PTY_ANSWER_MAX: usize = 3_000;
/// Answer bytes a message carries to any other endpoint.
const ANSWER_MAX: usize = 6_000;
/// Path bytes a pty copy carries (a very long PM dir cannot crowd out
/// the answer).
const PTY_PATH_MAX: usize = 512;
/// Bytes [`clip`]'s cut marker adds.
const CLIP_MARK_MAX: usize = 48;
/// The message source an answer is queued under.
pub(super) const SOURCE: &str = "answer";

/// The message id an answer is queued under — one per answer file, in
/// the daemon's `sys-` namespace (CAD-445).
pub(super) fn message_id(project: &str, issue: &str, report: &str) -> String {
    let path = format!("{project}/{issue}/{}/{report}", task_report::DIR);
    let hash: String = Sha256::digest(path.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sys-answer-{hash}")
}

/// Serialises the check-then-record of `answer_undeliverable`, so one
/// answer is recorded once however many routes race.
static UNDELIVERABLE: Mutex<()> = Mutex::new(());

/// `text` cut to at most `max` bytes on a char boundary, marked when cut.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} …[cut — the report has the rest]", &text[..end])
}

/// Characters a one-line paste never carries: controls, the Unicode
/// line/paragraph separators, and bidi embeddings/overrides/isolates
/// (which could make the pasted line read differently than it is).
fn unsafe_in_line(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// The message the asker receives. A pty pane pastes one line under
/// [`crate::adapter::pty::MAX_BODY`], so its copy is flattened and the
/// answer gets whatever the (bounded) header leaves; everyone else gets
/// the answer as written.
fn compose(
    issue: &str,
    by: &str,
    question: &str,
    answer: &str,
    path: &str,
    one_line: bool,
) -> String {
    if !one_line {
        return format!(
            "[answer] {issue}: {by} answered your question {question}.\nReport: {path} \
             (`cadence issue show {issue}`)\n\n{}",
            clip(answer.trim(), ANSWER_MAX)
        );
    }
    let flat = |t: &str| -> String {
        t.trim()
            .chars()
            .map(|c| if unsafe_in_line(c) { ' ' } else { c })
            .collect()
    };
    let head = format!("[answer] {issue}: {by} answered your question {question}: ");
    let tail = format!(
        " — full answer: {} (`cadence issue show {issue}`)",
        clip(&flat(path), PTY_PATH_MAX)
    );
    let room = crate::adapter::pty::MAX_BODY
        .saturating_sub(head.len() + tail.len() + CLIP_MARK_MAX)
        .min(PTY_ANSWER_MAX);
    format!("{}{}{tail}", flat(&head), clip(&flat(answer), room))
}

impl Shared {
    /// `answer_route` — see the module docs. Every refusal happens
    /// before anything is queued or recorded.
    pub(super) fn rpc_answer_route(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        // Only the two names; anything identity-shaped (`by`, `alias`,
        // `agent`, …) is refused, never read.
        if let Some(obj) = params.as_object() {
            if let Some(extra) = obj
                .keys()
                .find(|k| !matches!(k.as_str(), "issue" | "report"))
            {
                return Err(Error::rejected(format!(
                    "answer route: request field '{extra}' is not accepted — the caller \
                     is the connection's, the rest is the answer report's"
                )));
            }
        }
        let issue = required_str(params, "issue")?;
        let report = required_str(params, "report")?;
        let (by, sender) = match self.agent_caller(peer_pid, "answer route")? {
            AgentCaller::Operator => ("operator".to_string(), store::Sender::Operator),
            AgentCaller::Agent(alias) => (alias.clone(), store::Sender::Agent(alias)),
        };
        let pm_dir = self.pm_dir()?;
        let pm = Pm::at(&pm_dir)?;
        let (project, dir) = write::issue_dir(&pm, issue)?;
        let rows = task_report::list(&dir, issue);
        let answer = rows
            .iter()
            .find(|r| r["name"].as_str() == Some(report) && r["error"].is_null())
            .filter(|r| r["kind"] == "answer")
            .ok_or_else(|| {
                Error::rejected(format!(
                    "{issue} has no answer report '{report}' — `cadence issue show {issue}` \
                     lists its reports"
                ))
            })?;
        let author = answer["agent"].as_str().unwrap_or_default();
        if author != by {
            return Err(Error::rejected(format!(
                "answer route refused: {report} is filed as '{author}', but this connection \
                 is '{by}' — only an answer's own author routes it (CAD-447)"
            )));
        }
        let question = answer["answers"].as_str().unwrap_or_default();
        let asked = rows
            .iter()
            .find(|r| {
                r["name"].as_str() == Some(question)
                    && r["kind"] == "question"
                    && r["error"].is_null()
            })
            .ok_or_else(|| {
                Error::rejected(format!(
                    "{report} answers '{question}', which is not a question report on {issue}"
                ))
            })?;
        let asker = asked["agent"].as_str().unwrap_or_default().to_string();
        // Only the question's first answer reaches the asker.
        let first = asked["answered_by"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_str);
        if first != Some(report) {
            return Ok(json!({"sent": false, "to": asker, "why": format!(
                "already answered — {question} was first answered by {}; a later answer \
                 is filed but not sent",
                first.unwrap_or("another report")
            )}));
        }
        let path = pm_dir
            .join(&project.key)
            .join(issue)
            .join(task_report::DIR)
            .join(report)
            .display()
            .to_string();
        let mid = message_id(&project.key, issue, report);
        let facts = json!({"issue": issue, "question": question, "answer": report,
                           "to": asker, "by": by, "message": mid});
        if asker == by {
            return Ok(json!({"sent": false, "to": asker, "why": "the answerer asked it"}));
        }
        let Some(target) = self.store.agent_opt(&asker)? else {
            let why = format!("no agent '{asker}' is registered to receive it");
            self.undeliverable(facts, &why);
            return Ok(json!({"sent": false, "to": asker, "undeliverable": why}));
        };
        let text = compose(
            issue,
            &by,
            question,
            answer["body"].as_str().unwrap_or_default(),
            &path,
            target.endpoint_kind == "pty",
        );
        // No `reply_to`: an answer owes nobody a report. The store's
        // enqueue dedupes on the id only for identical content.
        let queued = self
            .store
            .enqueue_sent(&asker, &text, None, &mid, SOURCE, None, &sender);
        let (duplicate, state) = match queued {
            Ok(q) => q,
            Err(e)
                if e.to_string()
                    .contains("already used with different content") =>
            {
                // A squat: another message holds the answer's id. Never a
                // silent duplicate — record it and refuse loudly.
                let why = format!(
                    "message id {mid} is already taken by a different message — {asker} \
                     was not told; the answer stands"
                );
                self.undeliverable(facts, &why);
                return Err(Error::rejected(format!("answer route: {why}")));
            }
            Err(e) => return Err(e),
        };
        self.notify_agent(&asker);
        self.wake();
        if !duplicate {
            let _ = self.store.event_public(&asker, "answer_routed", facts);
        }
        Ok(json!({"sent": !duplicate, "to": asker, "message": mid,
                  "duplicate": duplicate, "state": state}))
    }

    /// Record once per answer (its message id) that it reached nobody.
    fn undeliverable(&self, facts: Value, why: &str) {
        let _one = UNDELIVERABLE.lock().unwrap_or_else(|e| e.into_inner());
        let mid = facts["message"].as_str().unwrap_or_default();
        if self
            .store
            .event_names_message(DAEMON_ALIAS, "answer_undeliverable", mid)
            .unwrap_or(false)
        {
            return;
        }
        let mut payload = facts;
        payload["why"] = json!(why);
        let _ = self
            .store
            .event_public(DAEMON_ALIAS, "answer_undeliverable", payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_id_is_one_per_answer_file() {
        let a = message_id("demo", "D-1", "20260924T000000Z-operator.md");
        assert_eq!(a, message_id("demo", "D-1", "20260924T000000Z-operator.md"));
        assert_ne!(a, message_id("demo", "D-1", "20260924T000001Z-operator.md"));
        assert_ne!(a, message_id("demo", "D-2", "20260924T000000Z-operator.md"));
        assert!(
            a.starts_with("sys-answer-") && a.len() == "sys-answer-".len() + 16,
            "{a}"
        );
        crate::proto::identifier(&a, "Message id").unwrap();
    }

    #[test]
    fn pty_copy_strips_separators_and_bidi_and_bounds_a_long_path() {
        let tricky: String = [
            'a', '\u{2028}', 'b', '\u{2029}', 'c', '\u{202E}', 'd', '\u{2067}', 'e',
        ]
        .iter()
        .collect();
        let text = compose("D-1", "operator", "q.md", &tricky, "/p", true);
        assert!(text.contains("a b c d e"), "{text}");
        assert!(!text.chars().any(unsafe_in_line), "{text:?}");
        let path = format!("/{}", "x".repeat(10_000));
        let text = compose("D-1", "operator", "q.md", &"y".repeat(10_000), &path, true);
        assert!(
            text.len() <= crate::adapter::pty::MAX_BODY,
            "{}",
            text.len()
        );
        assert!(text.contains("yyyy"), "the answer keeps room: {text}");
    }

    #[test]
    fn pty_copy_is_one_line_and_bounded() {
        let long = format!("line one\nline two\r\n{}", "é".repeat(4_000));
        let text = compose(
            "D-1",
            "operator",
            "q.md",
            &long,
            "/pm/D-1/reports/a.md",
            true,
        );
        assert!(!crate::adapter::pty::has_control_chars(&text), "{text}");
        assert!(
            text.len() <= crate::adapter::pty::MAX_BODY,
            "{}",
            text.len()
        );
        assert!(text.contains("line one line two"), "{text}");
        assert!(text.contains("/pm/D-1/reports/a.md"), "{text}");
        let full = compose(
            "D-1",
            "operator",
            "q.md",
            "Go with a.\nThen b.",
            "/p",
            false,
        );
        assert!(full.contains("Go with a.\nThen b."), "{full}");
        assert!(full.contains("Report: /p"), "{full}");
    }
}
