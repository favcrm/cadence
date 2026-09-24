//! CAD-445: the master wakes when work can move on, so the operator never
//! has to chat "go ahead" again. Three events each queue one system
//! message to the master:
//!
//! - `plan_approved` — the operator approved a plan (`plan_approve`);
//! - `delivery_merged` / `delivery_closed` / `delivery_declined` — a
//!   ticket's review loop ended (`delivery_observe`, `delivery_decline`);
//! - `blocker_done` — a ready ticket of an approved plan whose every
//!   `blocked_by` is now done or dropped, found by the report router's
//!   pass (tracker status is written by the CLI, not through the daemon,
//!   so the daemon notices it by reading the tracker).
//!
//! Each wake lists what is ready to dispatch now and what still waits,
//! read from the tracker at that moment; the master then dispatches with
//! its one verb, `master_dispatch`, which re-checks everything. A wake
//! grants nothing: it is a message, queued the way routed reports are,
//! and the master's allowlist and every caller rule are unchanged.
//!
//! **Once.** A wake's message id is [`proto::daemon_message_id`] of
//! `(event, ticket, revision)` — the plan's decision time, the loop
//! record's dispatch time, the ticket's blocker set — so a replay, a
//! second router pass or a daemon restart finds it queued already. Only
//! the daemon writes `sys-` ids ([`proto::caller_message_id`]).
//!
//! **A master that is not running.** The wake is queued in the master's
//! mailbox like any message and delivered when the master next runs
//! (`agent resume master`, or an auto-stopped master's resume) — never
//! written into a live input it does not have. Dropping it would recreate
//! the stall this exists to end, and a late wake is harmless: it says
//! what to check, and `master_dispatch` refuses anything no longer ready.
//! With no master registered at all there is no mailbox, so nothing is
//! queued; a master started later still hears of every ticket that is
//! ready by then — `blocker_done` is found from tracker state, and the
//! master's briefing tells it to look at the board.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};

use super::{Shared, DAEMON_ALIAS};
use crate::delivery::{self, Record, State};
use crate::error::Result;
use crate::issue::{self, board};
use crate::master::{self, wake_id, ALIAS};
use crate::proto;
use crate::store;

/// Most tickets one wake lists per line.
const LIST_MAX: usize = 20;
/// Most `blocker_done` wakes one router pass queues — one master turn
/// each; the rest are found again next pass.
const WAKES_PER_PASS: usize = 5;

fn finished(status: &str) -> bool {
    matches!(status, "done" | "dropped")
}

/// A plan's tickets as the master sees them: ready to dispatch now, and
/// ready but still waiting on a blocker.
struct Outlook {
    now: Vec<String>,
    waiting: Vec<String>,
}

/// Every `ready` ticket of an approved plan (`project` only, when given),
/// with the blockers it still waits on (`"D-2 is doing"`) — none when it
/// can be dispatched now.
fn plan_ready<'a>(
    pm_dir: &std::path::Path,
    issues: &'a [board::Issue],
    project: Option<&str>,
) -> Vec<(&'a board::Issue, Vec<String>)> {
    let status: HashMap<&str, &str> = issues
        .iter()
        .map(|i| (i.front.id.as_str(), i.front.status.as_str()))
        .collect();
    issues
        .iter()
        .filter(|i| {
            i.front.status == "ready"
                && i.front.plan_epic.is_some()
                && project.is_none_or(|p| p == i.project)
                && issue::plan::gate_master(pm_dir, &i.front, &i.body).is_ok()
        })
        .map(|i| {
            let open = i
                .front
                .blocked_by
                .iter()
                .filter_map(|b| {
                    let s = status.get(b.as_str()).copied().unwrap_or("missing");
                    (!finished(s)).then(|| format!("{b} is {s}"))
                })
                .collect();
            (i, open)
        })
        .collect()
}

fn outlook(pm_dir: &std::path::Path, issues: &[board::Issue], project: &str) -> Outlook {
    let mut out = Outlook {
        now: vec![],
        waiting: vec![],
    };
    for (i, open) in plan_ready(pm_dir, issues, Some(project)) {
        let f = &i.front;
        if open.is_empty() {
            out.now.push(match &f.owner {
                Some(o) => format!("{} ({o})", f.id),
                None => format!("{} (names no agent — pass --to)", f.id),
            });
        } else {
            out.waiting
                .push(format!("{} (waits: {})", f.id, open.join(", ")));
        }
    }
    out
}

fn list(items: &[String]) -> String {
    let mut s = items
        .iter()
        .take(LIST_MAX)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > LIST_MAX {
        s.push_str(&format!(", and {} more", items.len() - LIST_MAX));
    }
    s
}

/// The wake's closing lines: what to do next.
fn next_steps(o: &Outlook) -> String {
    let mut s = if o.now.is_empty() {
        "Nothing is ready to dispatch now.".to_string()
    } else {
        format!("Ready to dispatch now: {}.", list(&o.now))
    };
    if !o.waiting.is_empty() {
        s.push_str(&format!("\nWaiting on a blocker: {}.", list(&o.waiting)));
    }
    if o.now.is_empty() {
        s.push_str("\nTell the operator where things stand.");
    } else {
        s.push_str(
            "\nDispatch each ready ticket with `cadence master dispatch <ID>`, then tell the \
             operator what you sent.",
        );
    }
    s
}

impl Shared {
    /// Queue a daemon-originated message to `alias` once per
    /// `(kind, key)`: its id is [`proto::daemon_message_id`], unattributed
    /// (a system entry in a thread), with `kind` as its source. `Some(id)`
    /// when queued now, `None` when it was queued before.
    pub(super) fn daemon_message(
        self: &Arc<Self>,
        alias: &str,
        kind: &str,
        key: &str,
        text: &str,
    ) -> Result<Option<String>> {
        let id = proto::daemon_message_id(kind, key);
        if self.store.message(&id)?.is_some() {
            return Ok(None);
        }
        let receipt = self.send_as(
            &json!({"alias": alias, "text": text, "message": id, "source": kind}),
            &|_| Ok(store::Sender::Unattributed),
        )?;
        Ok((receipt["duplicate"] != json!(true)).then_some(id))
    }

    /// Wake the master once for `(event, key)`; `true` when queued now.
    /// A failure is logged, never returned: the operator's decision that
    /// caused it already stands.
    fn wake_master(self: &Arc<Self>, event: &str, subject: &str, key: &str, text: &str) -> bool {
        if !self.master_exists() {
            return false;
        }
        let text = format!("[wake] {text}");
        let (kind, key) = master::wake_key(event, key);
        match self.daemon_message(ALIAS, kind, &key, &text) {
            Ok(Some(id)) => {
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "master_woken",
                    json!({"event": event, "issue": subject, "message": id}),
                );
                true
            }
            Ok(None) => false,
            Err(e) => {
                tracing::warn!("master wake {event} {subject}: {e}");
                false
            }
        }
    }

    /// Every issue of the tracker, for a wake's outlook; `None` when the
    /// tracker cannot be read (the wake then says so).
    fn wake_issues(&self) -> Option<(std::path::PathBuf, Vec<board::Issue>)> {
        let pm_dir = self.pm_dir().ok()?;
        let issues = board::load_all(&pm_dir, None).ok()?;
        Some((pm_dir, issues))
    }

    fn wake_outlook(&self, project: &str) -> String {
        match self.wake_issues() {
            Some((pm_dir, issues)) => next_steps(&outlook(&pm_dir, &issues, project)),
            None => "The tracker could not be read — check `cadence issue ls --status ready`."
                .to_string(),
        }
    }

    /// `plan_approve` succeeded (`out` is `decide_plan`'s answer).
    pub(super) fn wake_on_plan_approved(self: &Arc<Self>, out: &Value) {
        let epic = out["epic"].as_str().unwrap_or_default();
        let project = out["project"].as_str().unwrap_or_default();
        let decided_at = out["decided_at"].as_str().unwrap_or_default();
        let text = format!(
            "plan {epic} approved by {} — its tickets are ready.\n{}",
            out["decided_by"].as_str().unwrap_or("operator"),
            self.wake_outlook(project)
        );
        let _ = self.wake_master(
            "plan_approved",
            epic,
            &format!("{epic}/{decided_at}"),
            &text,
        );
    }

    /// A review-loop record just entered a terminal state.
    pub(super) fn wake_on_delivery_end(self: &Arc<Self>, rec: &Record) {
        let pr = rec
            .pr
            .as_deref()
            .and_then(delivery::pr_ref)
            .map(|p| format!(" ({p})"))
            .unwrap_or_default();
        let (event, what) = match rec.state {
            State::Merged => ("delivery_merged", format!("{} merged{pr}.", rec.issue)),
            State::Closed => (
                "delivery_closed",
                format!("{}'s PR{pr} was closed without merging.", rec.issue),
            ),
            State::Declined => (
                "delivery_declined",
                format!(
                    "the operator declined {}'s merge{pr}: {}",
                    rec.issue,
                    rec.note.as_deref().unwrap_or("no reason given")
                ),
            ),
            _ => return,
        };
        let text = format!("{what}\n{}", self.wake_outlook(&rec.project));
        let _ = self.wake_master(
            event,
            &rec.issue,
            &format!("{}/{}", rec.issue, rec.dispatched_at),
            &text,
        );
    }

    /// One router pass (CAD-445): every ready ticket of an approved plan
    /// whose blockers are all done or dropped wakes the master once per
    /// blocker set. Returns how many wakes were queued.
    pub(super) fn route_wakes(self: &Arc<Self>) -> Result<usize> {
        if !self.master_exists() {
            return Ok(0);
        }
        let Some((pm_dir, issues)) = self.wake_issues() else {
            return Ok(0);
        };
        let unblocked: Vec<&board::Issue> = plan_ready(&pm_dir, &issues, None)
            .into_iter()
            .filter(|(i, open)| open.is_empty() && !i.front.blocked_by.is_empty())
            .map(|(i, _)| i)
            .collect();
        let mut woken = 0;
        for i in unblocked {
            if woken == WAKES_PER_PASS {
                break;
            }
            let mut blockers = i.front.blocked_by.clone();
            blockers.sort();
            blockers.dedup();
            let key = format!("{}/{}", i.front.id, blockers.join(","));
            if self
                .store
                .message(&wake_id("blocker_done", &key))?
                .is_some()
            {
                continue;
            }
            let text = format!(
                "{} is ready to dispatch — its blockers are done or dropped: {}.\n{}",
                i.front.id,
                blockers.join(", "),
                next_steps(&outlook(&pm_dir, &issues, &i.project))
            );
            if self.wake_master("blocker_done", &i.front.id, &key, &text) {
                woken += 1;
            }
        }
        Ok(woken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_steps_names_ready_and_waiting_tickets() {
        let o = Outlook {
            now: vec!["D-2 (w1)".into()],
            waiting: vec!["D-3 (waits: D-2 is doing)".into()],
        };
        let s = next_steps(&o);
        assert!(s.contains("Ready to dispatch now: D-2 (w1)."), "{s}");
        assert!(s.contains("Waiting on a blocker: D-3 (waits: D-2 is doing)."));
        assert!(s.contains("cadence master dispatch <ID>"));
        let none = next_steps(&Outlook {
            now: vec![],
            waiting: vec![],
        });
        assert!(none.starts_with("Nothing is ready"), "{none}");
    }

    #[test]
    fn list_is_bounded() {
        let items: Vec<String> = (0..25).map(|n| format!("D-{n}")).collect();
        let s = list(&items);
        assert!(s.ends_with("and 5 more"), "{s}");
        assert!(!s.contains("D-20,"), "{s}");
    }
}
