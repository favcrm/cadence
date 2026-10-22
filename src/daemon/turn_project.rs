//! CAD-1752: the one resolver for "which project is this agent working in".
//!
//! An agent hired from the catalogue lives in `<state>/agents/<alias>/home`,
//! which is no project repo, so a cwd lookup finds nothing. The project of a
//! turn comes from the task the turn carries: the issue of the running (else
//! the single queued) dispatched message (the kickoff names the lane worktree). An agent with no
//! such task falls back to its cwd, so an agent joined inside a repo behaves
//! as before; a catalogue agent never falls back to a
//! cwd and, with no task, has no project.

use std::path::Path;

use super::Shared;
use crate::store::Agent;

pub(super) struct TurnProject {
    pub key: String,
}

impl Shared {
    pub(super) fn turn_project(&self, agent: &Agent) -> Option<TurnProject> {
        let pm_dir = self.pm_dir().ok()?;
        let messages = self.store.active_messages(&agent.alias).unwrap_or_default();
        let hire = agent
            .params
            .as_ref()
            .is_some_and(|params| params.get("catalogue").is_some());
        let key_of = |issue: &str| crate::issue::project::key_for_issue(&pm_dir, issue);
        // The turn in flight decides: a running (or submitting) message's
        // own issue, and a ticketless one has no project however much is
        // queued behind it. `unknown` is neither running nor queued work.
        if let Some(live) = messages
            .iter()
            .find(|m| matches!(m.state.as_str(), "running" | "submitting"))
        {
            match live.issue.as_deref() {
                Some(issue) => return key_of(issue).map(|key| TurnProject { key }),
                None if hire => return None,
                None => {}
            }
        } else {
            // Nothing runs: queued dispatches count only when they all
            // point at one project, never a guess between two.
            let queued: Vec<Option<String>> = messages
                .iter()
                .filter(|m| m.state == "queued")
                .filter_map(|m| m.issue.as_deref())
                .map(key_of)
                .collect();
            if let Some(Some(first)) = queued.first() {
                if queued.iter().all(|key| key.as_ref() == Some(first)) {
                    return Some(TurnProject { key: first.clone() });
                }
                return None;
            }
            if !queued.is_empty() {
                return None;
            }
        }
        if agent
            .params
            .as_ref()
            .is_some_and(|params| params.get("catalogue").is_some())
        {
            return None;
        }
        crate::issue::project::key_for_cwd(&pm_dir, Path::new(&agent.cwd))
            .map(|key| TurnProject { key })
    }
}
