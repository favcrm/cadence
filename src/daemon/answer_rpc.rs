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
//!   same ticket; the message goes only to that question's recorded
//!   author;
//! - the message id is derived from the answer's path, so a retried
//!   answer (the tracker returns the same file for identical content)
//!   or a concurrent second call queues nothing new;
//! - an asker that is no longer registered is recorded as
//!   `answer_undeliverable` on the daemon stream; the answer itself
//!   stands.

use std::sync::Arc;

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
/// The message source an answer is queued under.
pub(super) const SOURCE: &str = "answer";

/// The message id an answer is queued under — one per answer file.
pub(super) fn message_id(project: &str, issue: &str, report: &str) -> String {
    let path = format!("{project}/{issue}/{}/{report}", task_report::DIR);
    let hash: String = Sha256::digest(path.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("answer-{hash}")
}

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

/// The message the asker receives. A pty pane pastes one line, so its
/// copy is flattened; everyone else gets the answer as written.
fn compose(
    issue: &str,
    by: &str,
    question: &str,
    answer: &str,
    path: &str,
    one_line: bool,
) -> String {
    if one_line {
        let flat: String = answer
            .trim()
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        format!(
            "[answer] {issue}: {by} answered your question {question}: {} — full answer: {path} \
             (`cadence issue show {issue}`)",
            clip(&flat, PTY_ANSWER_MAX)
        )
    } else {
        format!(
            "[answer] {issue}: {by} answered your question {question}.\nReport: {path} \
             (`cadence issue show {issue}`)\n\n{}",
            clip(answer.trim(), ANSWER_MAX)
        )
    }
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
            let mut payload = facts;
            payload["why"] = json!(why);
            let _ = self
                .store
                .event_public(DAEMON_ALIAS, "answer_undeliverable", payload);
            return Ok(json!({"sent": false, "to": asker, "undeliverable": why}));
        };
        if self.store.message(&mid)?.is_some() {
            return Ok(json!({"sent": false, "to": asker, "message": mid, "duplicate": true}));
        }
        let text = compose(
            issue,
            &by,
            question,
            answer["body"].as_str().unwrap_or_default(),
            &path,
            target.endpoint_kind == "pty",
        );
        let receipt = self.send_as(
            &json!({"alias": asker, "text": text, "message": mid, "source": SOURCE}),
            &|_| Ok(sender.clone()),
        )?;
        let duplicate = receipt["duplicate"].as_bool().unwrap_or(false);
        if !duplicate {
            let _ = self.store.event_public(&asker, "answer_routed", facts);
        }
        Ok(json!({"sent": !duplicate, "to": asker, "message": mid,
                  "duplicate": duplicate, "state": receipt["state"]}))
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
            a.starts_with("answer-") && a.len() == "answer-".len() + 16,
            "{a}"
        );
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
