//! `dispatch_send` (CAD-378 R6): the only plain-message path that
//! writes a message's `issue`/`worktree` lane tags — `agent_send` and
//! `message send` refuse them outright, because a caller-supplied lane
//! let a worker's own PM mark mail with a forged dispatch claim
//! (rev-260's round-5 finding).
//!
//! The tags are never taken from the request: the daemon resolves the
//! lane itself — the issue's single open `worktree` ref, kept only when
//! the dir exists, is a real worktree of one of the project's declared
//! repos, and lives under that repo's worktrees dir — exactly the lane
//! `issue start` just created or reused inside `dispatch::run`. The
//! caller's `worktree`, when sent, is corroboration only: it must name
//! the same dir or the whole send refuses.
//!
//! Who may call it: the caller the steer gate would admit — the
//! target's own PM or the operator (`may_mutate_agent` `Steer`). On top
//! of that, an agent caller must share a name with the issue's holders
//! — the dispatching PM or the lane's worker — when the issue is in a
//! protected status, mirroring `dispatch`'s claim check so a foreign
//! PM cannot mint a dispatch-shaped record on an issue another lane
//! holds. The operator dispatches and repairs regardless.
//!
//! `--job` kickoffs keep going through `task_dispatch`, whose task/job
//! rows are already daemon-owned.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use super::{optional_str, reject_identity_fields, required_str, Shared};
use crate::error::{Error, Result};
use crate::issue::{self, claim};
use crate::peer::{may_mutate_agent, AgentCaller, AgentMutation};
use crate::store;

impl Shared {
    /// `dispatch_send` — enqueue `alias`'s kickoff for `issue` with the
    /// daemon-resolved lane tags. Only `dispatch::run` (including the
    /// master's in-daemon dispatch) calls it; its caller rule is the
    /// steer gate plus the claim check below.
    pub(super) fn rpc_dispatch_send(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        const VERB: &str = "dispatch send";
        reject_identity_fields(params, VERB)?;
        // A kickoff never steers or tasks — the fields that would make
        // this a different kind of send are refused, not ignored.
        for field in ["task", "priority", "supersedes", "nudge", "source"] {
            if params.get(field).is_some_and(|v| !v.is_null()) {
                return Err(Error::rejected(format!(
                    "{VERB} takes no `{field}` — a dispatch kickoff is a plain \
                     lane-bound send; steering and task binding do not apply"
                )));
            }
        }
        // `take_over` is refused rather than honored: `issue start`
        // records a take-over under the tracker lock before this send
        // runs, so accepting one here would let it pass unrecorded.
        if optional_str(params, "take_over").is_some() {
            return Err(Error::rejected(format!(
                "{VERB} takes no `take_over` — a take-over is recorded by \
                 `issue start` before the kickoff, never by the send"
            )));
        }
        let caller = self.agent_caller(peer_pid, VERB)?;
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let id = issue::model::check_id(required_str(params, "issue")?)?;
        let text = required_str(params, "text")?;
        let reply_to = required_str(params, "reply_to")?;
        let mid = optional_str(params, "message")
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let target = self
            .store
            .agent_opt(&alias)?
            .ok_or_else(|| Error::rejected(format!("{VERB}: unknown agent '{alias}'")))?;
        if target.endpoint_kind == "pty" && crate::adapter::pty::has_control_chars(text) {
            return Err(Error::rejected(
                "PTY messages must be a single line without control characters \
                 — put a long body in a file and send its path",
            ));
        }
        // The steer gate, kept: dispatch marks a worker's queue, so the
        // caller must be the operator or the worker's own PM.
        let pm_of = self.effective_pm(&target)?;
        may_mutate_agent(
            &caller,
            &alias,
            pm_of.as_deref(),
            AgentMutation::Steer,
            VERB,
        )
        .map_err(Error::rejected)?;
        let pm = self.pm()?;
        let (project, dir) = issue::write::issue_dir(&pm, &id)?;
        let (front, _) = issue::write::load_front(&dir)?;
        // The dispatch-side claim check `dispatch::run` runs before
        // `issue start`, repeated here against the connection-bound
        // caller (never `reply_to`, which is a request field): an issue
        // doing/review held by names disjoint from {caller, worker}
        // refuses, so a foreign PM cannot mint a dispatch-shaped record
        // on a lane someone else holds. The operator's repairs and
        // hand-dispatches pass --reply-to as the requester it speaks
        // for, as the CLI's own check does.
        let requesters: Vec<&str> = match &caller {
            AgentCaller::Agent(a) => vec![a.as_str(), alias.as_str()],
            AgentCaller::Operator => vec![reply_to, alias.as_str()],
        };
        claim::check(&front, &requesters, None, VERB, || {
            claim::since(&pm.dir, &project.key, &front, Duration::from_secs(2))
        })?;
        let worktree = self.dispatch_lane(VERB, &project, &front)?;
        if let Some(claimed) = optional_str(params, "worktree") {
            let want = Path::new(claimed)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(claimed));
            if want != worktree {
                return Err(Error::rejected(format!(
                    "{VERB}: '{claimed}' is not the lane {id} has open — the \
                     daemon resolves the tag from the issue's own record and \
                     got {}; a dispatch whose start resolved elsewhere is \
                     refused, not re-pointed",
                    worktree.display()
                )));
            }
        }
        let sender = self.thread_sender(&alias, peer_pid)?;
        let (duplicate, state) = self.store.enqueue_steered(
            &alias,
            text,
            Some(reply_to),
            &mid,
            "dispatch",
            None,
            Some(&id),
            Some(&worktree.to_string_lossy()),
            &sender,
            &store::Steer::NONE,
        )?;
        self.notify_agent(&alias);
        self.wake();
        let mut receipt = json!({"message": mid, "state": state, "duplicate": duplicate});
        if let Some(warning) = self.inbox_warning(&alias) {
            receipt["warning"] = json!(warning);
        }
        Ok(receipt)
    }

    /// The lane a dispatch binds, resolved daemon-side: the issue's
    /// open `worktree` refs, kept only while each still exists on disk
    /// as a worktree of one of the project's declared repos under that
    /// repo's worktrees dir. Exactly one must resolve — none means
    /// `issue start` never ran (or its lane is gone), several means the
    /// tracker names competing lanes and a pick would be a guess.
    fn dispatch_lane(
        &self,
        verb: &str,
        project: &issue::project::Project,
        front: &issue::model::Front,
    ) -> Result<PathBuf> {
        let repos = issue::start::declared_repos(project);
        let mut lanes: Vec<PathBuf> = Vec::new();
        for cand in issue::start::open_worktrees(front) {
            let dir = cand.canonicalize().unwrap_or(cand.clone());
            let Ok(root) = crate::worktree::main_root(&dir) else {
                continue;
            };
            if !repos.contains(&root) {
                continue;
            }
            let wt_root = crate::worktree::layout::worktrees_dir(&root)
                .canonicalize()
                .unwrap_or_else(|_| crate::worktree::layout::worktrees_dir(&root));
            if dir.starts_with(&wt_root) {
                lanes.push(dir);
            }
        }
        lanes.sort();
        lanes.dedup();
        match lanes.len() {
            1 => Ok(lanes.remove(0)),
            0 => Err(Error::rejected(format!(
                "{verb}: {} has no open lane the daemon can verify — a dispatch \
                 kickoff binds the worktree `issue start` recorded; none of its \
                 open worktree refs is a live worktree of the project's repos",
                front.id
            ))),
            n => Err(Error::rejected(format!(
                "{verb}: {} has {n} open lane refs the daemon can verify — \
                 which one a kickoff binds is ambiguous; close the stale \
                 lanes first",
                front.id
            ))),
        }
    }
}
