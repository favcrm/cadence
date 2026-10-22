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
        let lanes = |running: bool| -> Vec<TurnProject> {
            let mut found: Vec<TurnProject> = Vec::new();
            for message in &messages {
                let live = matches!(message.state.as_str(), "running" | "submitting");
                let Some(issue) = message.issue.as_deref().filter(|_| live == running) else {
                    continue;
                };
                let Some(key) = crate::issue::project::key_for_issue(&pm_dir, issue) else {
                    continue;
                };
                found.push(TurnProject { key });
            }
            found
        };
        // The turn in flight decides; queued work only when it is
        // unambiguous (one project), never a guess between two.
        let mut running = lanes(true);
        if !running.is_empty() {
            return Some(running.swap_remove(0));
        }
        let queued = lanes(false);
        if let Some(first) = queued.first() {
            if queued.iter().all(|lane| lane.key == first.key) {
                return queued.into_iter().next();
            }
            return None;
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
