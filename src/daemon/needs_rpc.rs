//! `needs_dismiss` (CAD-574): the operator snoozes or dismisses a
//! Needs-you row by subject. Operator-only by the connection
//! ([`Shared::operator_connection`]) — an agent caller, its detached
//! child, and any identity-shaped request field are refused before
//! anything is written; the board relays it operator-only too.
//!
//! The record lands in the daemon's `needs_dismissed.json`
//! ([`crate::needs_dismiss`]) and every overview build applies it, so
//! the row leaves the rail for all readers at once. `dismiss` names
//! the occurrence the operator saw — the row returns if its condition
//! re-occurs (a newer `since` beats the record's `at`); `snooze` names
//! a window — `24h` or `7d` — after which the row returns regardless.

use serde_json::{json, Value};

use super::{optional_u64, required_str, Shared, DAEMON_ALIAS};
use crate::error::{Error, Result};
use crate::needs_dismiss;

/// The snooze windows the verb accepts — the rail's two choices.
const SNOOZE_SECS: &[u64] = &[86_400, 604_800];

/// Subject `kind` grammar: identifier-charset, bounded — `agent`,
/// `issue`, `pr`, `repo`, `deploy`, `tracker`, `report`, `row`,
/// `monitor`, `ci` are the kinds overview rows carry.
fn subject_kind(params: &Value) -> Result<&str> {
    let kind = required_str(params, "kind")?;
    if kind.is_empty()
        || kind.len() > 40
        || !kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(Error::rejected(format!(
            "needs dismiss: bad subject kind '{kind}' — [A-Za-z0-9._-], 1-40 chars"
        )));
    }
    Ok(kind)
}

/// Subject `id` — a row id can carry a title (`row` subjects), so any
/// printable text within the cap; control characters refuse.
fn subject_id(params: &Value) -> Result<&str> {
    let id = required_str(params, "id")?;
    if id.is_empty() || id.len() > 240 || id.chars().any(char::is_control) {
        return Err(Error::rejected(
            "needs dismiss: bad subject id — 1-240 chars, no control characters",
        ));
    }
    Ok(id)
}

impl Shared {
    /// `needs_dismiss` — `{kind, id, mode: "snooze"|"dismiss", secs?}`.
    /// Answers the record it wrote.
    pub(super) fn rpc_needs_dismiss(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        const VERB: &str = "needs dismiss";
        self.operator_connection(VERB, params, peer_pid)?;
        let kind = subject_kind(params)?.to_string();
        let id = subject_id(params)?.to_string();
        let mode = required_str(params, "mode")?;
        let secs = optional_u64(params, "secs");
        let until = match mode {
            "snooze" => {
                let Some(secs) = secs.filter(|s| SNOOZE_SECS.contains(s)) else {
                    return Err(Error::rejected(format!(
                        "{VERB}: a snooze takes secs: {} — nothing else",
                        SNOOZE_SECS
                            .iter()
                            .map(u64::to_string)
                            .collect::<Vec<_>>()
                            .join(" or ")
                    )));
                };
                Some(secs as i64)
            }
            "dismiss" => {
                if secs.is_some() {
                    return Err(Error::rejected(
                        "{VERB}: a dismissal takes no window — it lifts when the \
                         row's condition re-occurs",
                    ));
                }
                None
            }
            other => {
                return Err(Error::rejected(format!(
                    "{VERB}: unknown mode '{other}' — 'snooze' or 'dismiss'"
                )))
            }
        };
        let now = crate::issue::time::now_epoch();
        let mut record = json!({"kind": kind, "id": id, "mode": mode, "at": now, "by": "operator"});
        if let Some(secs) = until {
            record["until"] = json!(now + secs);
        }
        needs_dismiss::record(&self.state_dir, record.clone(), now)?;
        let _ = self
            .store
            .event_public(DAEMON_ALIAS, "needs_dismissed", record.clone());
        self.wake();
        Ok(record)
    }
}
