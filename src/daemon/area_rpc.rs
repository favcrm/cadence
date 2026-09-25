//! `area_ack` (CAD-378): the owner of a code area acks a lane's change
//! to it, clearing the Needs-you `area_ack` row.
//!
//! The acker is bound from the connection ([`Shared::agent_caller`]):
//! the proven operator, or the agent whose alias is the area's owner PM
//! in the project's `PROJECT.md` `areas:`. Any other agent — the lane's
//! own worker included — a detached child of the owner (no identity,
//! not provably the operator) and identity-shaped request fields are
//! refused before anything is written. The ack is recorded in the
//! daemon's state dir (`area_acks.json`) plus an `area_acked` event —
//! never a tracker comment, whose author any writer can claim.

use serde_json::{json, Value};

use super::{optional_str, reject_identity_fields, required_str, Shared, DAEMON_ALIAS};
use crate::error::{Error, Result};
use crate::issue::{self, areas, Pm};
use crate::peer::AgentCaller;

/// Longest ack note.
const NOTE_MAX: usize = 500;

impl Shared {
    pub(super) fn rpc_area_ack(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        const VERB: &str = "area ack";
        reject_identity_fields(params, VERB)?;
        let caller = self.agent_caller(peer_pid, VERB)?;
        let id = issue::model::check_id(required_str(params, "issue")?)?;
        let name = required_str(params, "area")?;
        let note = optional_str(params, "note").filter(|n| !n.trim().is_empty());
        if note.is_some_and(|n| n.len() > NOTE_MAX || n.chars().any(char::is_control)) {
            return Err(Error::rejected(format!(
                "{VERB}: note must be one line of at most {NOTE_MAX} bytes"
            )));
        }
        let pm = Pm::at(&self.pm_dir()?)?;
        let (project, _) = issue::write::issue_dir(&pm, &id)?;
        let all = areas::load(&pm.dir, &project.key)?;
        let area = all.iter().find(|a| a.name == name).ok_or_else(|| {
            Error::rejected(format!(
                "{VERB}: project {} declares no area '{name}' — known: {}",
                project.key,
                if all.is_empty() {
                    "none".to_string()
                } else {
                    all.iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ))
        })?;
        let by = match caller {
            AgentCaller::Operator => "operator".to_string(),
            AgentCaller::Agent(alias) if area.pm.as_deref() == Some(alias.as_str()) => alias,
            AgentCaller::Agent(alias) => {
                return Err(Error::rejected(format!(
                    "{VERB}: area '{name}' is owned by {} — only its owner PM{} or the \
                     operator may ack; this connection is agent '{alias}'",
                    area.owner(),
                    area.pm
                        .as_deref()
                        .map(|p| format!(" ({p})"))
                        .unwrap_or_default()
                )));
            }
        };
        // Pin the ack to the lane the daemon's dispatch record bound —
        // its recorded worktree and branch, never a live frontmatter
        // ref an agent can re-point. Both reads are object-only; a
        // lane whose recorded dir is unreadable acks `head: null`,
        // which pins nothing — the row stays up. An unbound lane has
        // nothing a daemon saw to pin: its row cannot be acked at all.
        let rec = areas::dispatches(&self.state_dir)
            .remove(&id)
            .ok_or_else(|| {
                Error::rejected(format!(
                    "{VERB}: {id} has no daemon-recorded dispatch — an unbound lane's \
                 row cannot be pinned or cleared; re-dispatch it or close the lane"
                ))
            })?;
        let wt = rec["worktree"].as_str().unwrap_or_default().to_string();
        let rev = rec["branch"].as_str().unwrap_or("HEAD").to_string();
        let head = areas::lane_head(std::path::Path::new(&wt), &rev).ok();
        let files: Vec<String> = areas::changed_files(std::path::Path::new(&wt), &rev)
            .map(|fs| fs.into_iter().filter(|f| area.covers(f)).collect())
            .unwrap_or_default();
        let record = json!({
            "issue": id,
            "area": name,
            "project": project.key,
            "owner": area.owner(),
            "by": by,
            "at": issue::time::iso(issue::time::now_epoch()),
            "head": head,
            "files": files,
            "note": note,
        });
        areas::record_ack(&self.state_dir, &areas::ack_key(&id, name), record.clone())?;
        let _ = self
            .store
            .event_public(DAEMON_ALIAS, "area_acked", record.clone());
        self.wake();
        Ok(record)
    }

    /// `dispatch_record` — the daemon's own record of a lane dispatch:
    /// which worktree the kickoff bound, which PM sent it, and to
    /// which worker. `message` names the kickoff the daemon delivered;
    /// every recorded field is derived from that message and the
    /// daemon's own rows — the task for a `--job` kickoff, the
    /// message's daemon-written `issue`/`worktree` plus the thread
    /// entry's connection-attributed sender for a plain one. Request
    /// fields never steer it: the `pm` is the send's recorded sender,
    /// and only that sender (or the operator) may write or replace the
    /// record — a caller-supplied worktree, branch or `pm` binds
    /// nothing and one record overwrites another only under the same
    /// PM or the operator (CAD-378 R3/R5). No tracker data is
    /// consulted: issue refs and kickoff body text are agent-writable
    /// and laundered the round-4 forgery.
    pub(super) fn rpc_dispatch_record(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        const VERB: &str = "dispatch_record";
        reject_identity_fields(params, VERB)?;
        let caller = self.agent_caller(peer_pid, VERB)?;
        let id = issue::model::check_id(required_str(params, "issue")?)?;
        let mid = required_str(params, "message")?;
        let pm = Pm::at(&self.pm_dir()?)?;
        issue::write::issue_dir(&pm, &id)?;
        let msg = self.store.message(mid)?.ok_or_else(|| {
            Error::rejected(format!(
                "{VERB}: '{mid}' is not a message the daemon delivered — \
                 the record names the kickoff it binds, nothing else"
            ))
        })?;
        let (owner, owner_kind, worktree, branch) = self.kickoff_binding(VERB, &id, &msg)?;
        // Only the kickoff's own sender may record it; the operator may
        // always (it dispatches by hand and repairs). A different agent
        // cannot write or overwrite this issue's binding.
        let allowed = match &caller {
            AgentCaller::Operator => true,
            AgentCaller::Agent(alias) => *alias == owner,
        };
        if !allowed {
            let (by, _) = caller.audit();
            return Err(Error::rejected(format!(
                "{VERB}: the kickoff {mid} was sent by '{owner}' — only that PM \
                 or the operator may record it; this connection is agent '{by}'"
            )));
        }
        let record = json!({
            "issue": id,
            "pm": owner,
            "pm_kind": owner_kind,
            "worker": msg.alias,
            "worktree": worktree,
            "branch": branch,
            "message": mid,
            "at": issue::time::iso(issue::time::now_epoch()),
        });
        areas::record_dispatch(
            &self.state_dir,
            &id,
            record.clone(),
            matches!(caller, AgentCaller::Operator),
        )?;
        let _ = self
            .store
            .event_public(DAEMON_ALIAS, "dispatch_recorded", record.clone());
        self.wake();
        Ok(record)
    }

    /// The `(pm, pm_kind, worktree, branch)` a delivered message binds
    /// for `dispatch_record` — derived, never requested. A task-bound
    /// message (`task_dispatch`) must BE the task's recorded dispatch
    /// kickoff, its job must name this issue, and it must have been
    /// delivered to the task's recorded worker; the lane comes from the
    /// task's row and the pm is the job's recorded PM. A plain kickoff
    /// must carry schema v16's daemon-written `issue`/`worktree` — set
    /// at send behind the steer gate, so only the worker's own PM or
    /// the operator can mark a send with a lane — and its pm is the
    /// sender the daemon attributed when it was queued (`reply_to`
    /// stands in only when the recipient keeps no thread entry). The
    /// kickoff body and tracker refs prove nothing — both are
    /// agent-writable — so none is read.
    fn kickoff_binding(
        &self,
        verb: &str,
        id: &str,
        msg: &crate::store::Message,
    ) -> Result<(String, &'static str, String, String)> {
        let (owner, owner_kind, worktree, branch) = if let Some(task_id) = &msg.task_id {
            let task = self.store.task(task_id)?;
            if task.dispatch_message.as_deref() != Some(msg.id.as_str()) {
                return Err(Error::rejected(format!(
                    "{verb}: message {} is bound to task {task_id} but is not its \
                     dispatch kickoff — a task note or report cannot anchor \
                     {id}'s record",
                    msg.id
                )));
            }
            // The kickoff went to the task's recorded worker — the lane
            // belongs to the assignee the dispatch named, and a kickoff
            // delivered to anyone else is not this lane's dispatch.
            if task.assignee.as_deref() != Some(msg.alias.as_str()) {
                return Err(Error::rejected(format!(
                    "{verb}: the dispatch kickoff {} was sent to '{}' but task \
                     {task_id} records assignee {:?} — it did not go to this \
                     lane's worker",
                    msg.id, msg.alias, task.assignee
                )));
            }
            let job = self.store.job(&task.job_id)?;
            if job.issue_id.as_deref() != Some(id) {
                return Err(Error::rejected(format!(
                    "{verb}: task {task_id}'s job {} is bound to issue {:?}, \
                     not {id}",
                    job.id, job.issue_id
                )));
            }
            let repo = job.repo.as_deref().ok_or_else(|| {
                Error::rejected(format!(
                    "{verb}: job {} records no repo — the task's worktree \
                     cannot be resolved",
                    job.id
                ))
            })?;
            let wt_name = task.worktree.as_deref().ok_or_else(|| {
                Error::rejected(format!(
                    "{verb}: task {task_id} records no worktree — the lane \
                     it bound is unknown"
                ))
            })?;
            let wt = crate::worktree::layout::worktree_dir(std::path::Path::new(repo), wt_name);
            (
                job.pm_alias.clone(),
                "job",
                wt.to_string_lossy().into_owned(),
                task.branch.as_deref().unwrap_or("HEAD").to_string(),
            )
        } else {
            // A plain kickoff binds the lane the daemon itself wrote on
            // the message at send time (schema v16): `issue` must name
            // THIS issue and `worktree` IS the bound lane — absolute,
            // like every `dispatch::run` send records it. A message
            // without them is just mail, however its body reads.
            if msg.issue.as_deref() != Some(id) {
                return Err(Error::rejected(format!(
                    "{verb}: message {} carries no daemon-written `issue` \
                     binding to {id} — it is not a dispatch kickoff for it \
                     and cannot anchor the record",
                    msg.id
                )));
            }
            let worktree = msg.worktree.clone().ok_or_else(|| {
                Error::rejected(format!(
                    "{verb}: message {} names {id} but carries no daemon-written \
                     `worktree` — a real dispatch kickoff binds the lane it \
                     was sent for",
                    msg.id
                ))
            })?;
            // The pm is the sender the daemon attributed at send time;
            // `reply_to` stands in only when the recipient keeps no
            // thread entry (unthreaded workers carry none) — an
            // attributed sender is never taken from a request field.
            let (owner, owner_kind) = match self.store.message_sender(&msg.id)? {
                Some((role, _)) if role == "operator" => ("operator".to_string(), "operator"),
                Some((_, Some(from))) => (from, "agent"),
                _ => match msg.reply_to.as_deref() {
                    Some(reply) => (reply.to_string(), "agent"),
                    None => {
                        return Err(Error::rejected(format!(
                            "{verb}: message {} carries no daemon-attributed sender \
                             and no reply_to — it cannot anchor {id}'s record",
                            msg.id
                        )))
                    }
                },
            };
            // v16 writes no branch for a plain send; the record probes
            // the recorded lane's checkout (`HEAD`).
            (owner, owner_kind, worktree, "HEAD".to_string())
        };
        issue::model::check_ref_value(&worktree)?;
        issue::model::check_ref_value(&branch)?;
        if !std::path::Path::new(&worktree).is_absolute() {
            return Err(Error::rejected(format!(
                "{verb}: the kickoff's worktree '{worktree}' is not an \
                 absolute path"
            )));
        }
        Ok((owner, owner_kind, worktree, branch))
    }
}
