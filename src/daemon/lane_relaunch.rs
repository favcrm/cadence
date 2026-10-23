//! CAD-1752: relaunch-per-ticket for a confined catalogue agent.
//!
//! A Landlock grant is fixed when the process launches, so a confined
//! agent hired without a repo (cwd = its home dir) cannot be granted a
//! ticket's worktree while it runs. Instead, between turns, the daemon
//! relaunches it with the delivered task's lane worktree as its working
//! dir: the grant is that worktree's set plus the agent's own dir, and the
//! same Pi session file is resumed. A ticketless message relaunches it back
//! home. Only the daemon sets `params.launch_workdir`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::*;

impl Shared {
    /// What the next delivery needs: `Ok(None)` when the agent is already
    /// where the message needs it (or is not subject to relaunching),
    /// `Ok(Some(workdir))` when it must be relaunched there, and an
    /// `Err(PreWrite)` when the message's worktree cannot be granted -
    /// nothing is relaunched or granted then.
    pub(super) fn lane_workdir_needed(
        &self,
        agent: &Agent,
        message: &store::Message,
    ) -> Result<Option<PathBuf>> {
        let is_hire = crate::agent_roles::is_catalogue_hire(agent.params.as_ref());
        if !is_hire
            || !adapter::registry::pi_family_managed(&agent.provider, &agent.endpoint_kind)
            || !adapter::pi::worker_confined(agent)
        {
            return Ok(None);
        }
        let home = PathBuf::from(&agent.cwd);
        let needed = match message.issue.as_deref() {
            None => home,
            Some(issue) => self.grantable_lane(issue, message)?,
        };
        Ok((needed != adapter::pi::launch_workdir(agent)).then_some(needed))
    }

    /// The lane worktree a dispatched message may be granted: the one the
    /// daemon recorded for the issue's dispatch, an existing directory
    /// under `<repo>/.cadence/wt/` of a repo the issue's project declares.
    fn grantable_lane(&self, issue: &str, message: &store::Message) -> Result<PathBuf> {
        let refuse = |why: String| {
            Error::PreWrite(format!(
                "relaunch for {issue} refused, nothing granted: {why}"
            ))
        };
        let tagged = message
            .worktree
            .as_deref()
            .filter(|w| !w.is_empty())
            .ok_or_else(|| refuse("the dispatch recorded no worktree".into()))?;
        let lane = Path::new(tagged)
            .canonicalize()
            .map_err(|_| refuse(format!("worktree {tagged} is missing or deleted")))?;
        if !lane.is_dir() {
            return Err(refuse(format!("worktree {tagged} is not a directory")));
        }
        // The daemon's dispatch record, not the agent-writable tracker,
        // names the lane for this issue.
        let records = crate::issue::areas::dispatches(&self.state_dir);
        let recorded = records
            .get(issue)
            .and_then(|r| r["worktree"].as_str())
            .and_then(|w| Path::new(w).canonicalize().ok());
        if recorded.as_deref() != Some(lane.as_path()) {
            return Err(refuse(format!(
                "{} is not the worktree recorded for {issue}'s dispatch",
                lane.display()
            )));
        }
        let pm_dir = self
            .pm_dir()
            .map_err(|_| refuse("the tracker is unreadable".into()))?;
        let key = crate::issue::project::key_for_issue(&pm_dir, issue)
            .ok_or_else(|| refuse(format!("{issue} belongs to no known project")))?;
        let project = crate::issue::project::list(&pm_dir)
            .ok()
            .and_then(|all| all.into_iter().find(|p| p.key == key))
            .ok_or_else(|| refuse(format!("project {key} is unreadable")))?;
        let inside = crate::issue::start::declared_repos(&project)
            .iter()
            .any(|repo| {
                let wt = repo.join(".cadence").join("wt");
                lane.starts_with(wt.canonicalize().unwrap_or(wt)) && lane != *repo
            });
        if !inside {
            return Err(refuse(format!(
                "{} is not a lane worktree (.cadence/wt/) of a repo of project {key}",
                lane.display()
            )));
        }
        Ok(lane)
    }

    /// Between turns: close the idle endpoint and reopen it in `workdir`,
    /// resuming the same Pi session file. The workdir is recorded for the
    /// launch (`params.launch_workdir`, absent = home) and as an event.
    pub(super) fn relaunch_for_lane(
        self: &Arc<Self>,
        ctl: &Arc<AgentCtl>,
        agent: &mut Agent,
        adapter: &mut Arc<dyn ProviderAdapter>,
        workdir: &Path,
        message: &store::Message,
    ) -> Result<()> {
        let alias = agent.alias.clone();
        let home = Path::new(&agent.cwd);
        let value = if workdir == home {
            Value::Null
        } else {
            json!(workdir.to_string_lossy())
        };
        self.store.set_params_by(
            &alias,
            &json!({ "launch_workdir": value }),
            &json!({"by": "daemon", "by_kind": "daemon"}),
        )?;
        if let Some(old) = ctl.adapter.lock().unwrap().take() {
            old.close();
        }
        *agent = self.store.agent(&alias)?;
        *adapter = self.open_actor_adapter(&alias, ctl, agent)?;
        // The fork carries every entry of the old session, so the new
        // session id is not a lost conversation: no continuity pack.
        {
            let mut due = self.continuity_due.lock().unwrap();
            if due.get(&alias) == Some(&crate::continuity::Reason::New) {
                due.remove(&alias);
            }
        }
        let _ = self.store.event_public(
            &alias,
            "relaunched_for_lane",
            json!({"issue": message.issue, "message": message.id,
                   "workdir": workdir.to_string_lossy()}),
        );
        Ok(())
    }
}
