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
        // Only a proven daemon self-peer can mark an automatic send.
        // The marker requests stricter validation; it never grants authority.
        let automatic = match params.get("unattended") {
            None => false,
            Some(Value::Bool(true))
                if peer_pid == std::process::id() && matches!(&caller, AgentCaller::Operator) =>
            {
                true
            }
            Some(_) => {
                return Err(Error::rejected(format!(
                    "{VERB}: `unattended` marks the daemon's own automatic \
                     dispatch — it is not a caller field"
                )));
            }
        };
        // Automatic callers retain their outer read guard across self-RPC;
        // do not reacquire it here while a grant writer may be waiting.
        let _grant = match &caller {
            AgentCaller::Agent(_) => Some(
                self.unattended_lock
                    .read()
                    .unwrap_or_else(|e| e.into_inner()),
            ),
            AgentCaller::Operator => None,
        };
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
        crate::adapter::pi::refuse_confined_devin_agent(&target)?;
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
        let issue_source = std::fs::read_to_string(dir.join("issue.md"))?;
        let (front, issue_body) = issue::parse::parse_issue(&issue_source)?;
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
        // CAD-622: an app's own team agents never author changes to that
        // app's source. This is the daemon's own admission boundary — the
        // caller is the connection-derived one (never a request field),
        // and the target worker is named by the request; both are authors
        // of the work the kickoff opens. The ticket's planned paths and
        // the project's registered repositories (an app's pinned git
        // source) are the evidence, and the store adds run membership.
        let app_paths =
            crate::issue::app_guard::apps_in_paths(front.paths.iter().map(String::as_str));
        let remotes: Vec<String> = project
            .repos
            .iter()
            .filter_map(|r| r.remote.clone())
            .collect();
        let caller_alias = match &caller {
            AgentCaller::Agent(a) => Some(a.as_str()),
            AgentCaller::Operator => None,
        };
        let mut authors: Vec<&str> = vec![alias.as_str()];
        authors.extend(caller_alias);
        let guard = crate::issue::app_guard::Ctx {
            pm_dir: &pm.dir,
            state_dir: Some(&self.state_dir),
            store: Some(&self.store),
        };
        if let Some(why) = crate::issue::app_guard::refusals(guard, &app_paths, &authors, &remotes)?
        {
            return Err(Error::rejected(why));
        }
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
        // CAD-1822: an already-accepted id is decided by the Store's exact
        // envelope comparison. A lost-ACK retry must see the known accepted
        // outcome, so it never re-captures project context or adds metadata;
        // only an existing pinned selection keeps its conversation real.
        let already_accepted = self.store.message_exists(&mid)?;
        // Keep an accepted pinned selection's native conversation real; a
        // legacy accepted mid gains no new metadata.
        if already_accepted && self.store.context_pin(&alias, &mid)?.is_some() {
            self.store.ensure_thread(&alias)?;
        }
        // CAD-1846: the sender's `--no-lessons` is daemon-side selection
        // policy, so it must reach the shared guard rather than being
        // re-derived here.
        let no_lessons = match params.get("no_lessons") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(Error::rejected(format!(
                    "{VERB} takes `no_lessons` as a boolean"
                )))
            }
        };
        // Derive the context selection for a genuinely new dispatch and pass
        // it into the store's admission transaction. The selection commits
        // only with an accepted NEW row: a rejected or duplicate envelope
        // writes nothing, so a colliding same-id request cannot retarget a
        // queued turn's context. The guard pins first: when the project is not
        // opted in it returns without this closure, so no source is read.
        let selection = if already_accepted {
            None
        } else {
            let context_target = issue::dispatch::ContextTarget {
                team_role: target.team_role.as_deref(),
                provider: &target.provider,
                endpoint_kind: &target.endpoint_kind,
            };
            let make = || {
                let (request, items) = issue::parse::task_request(issue_body);
                let contract = crate::context_pack::TaskContract::new(
                    &front.id,
                    &front.title,
                    issue_source.as_bytes(),
                    &items,
                    request,
                );
                Ok((contract, std::borrow::Cow::Borrowed(text)))
            };
            match issue::dispatch::context_guard(
                &pm,
                &project,
                &front,
                context_target,
                no_lessons,
                make,
            )? {
                Some(prepared) => {
                    // Opted-in delivery shares the alias's native home
                    // conversation from the first send onward: the enqueue
                    // note, provider compaction notes and later receipts must
                    // name one real thread id.
                    self.store.ensure_thread(&alias)?;
                    Some(store::IssueSelection::Pin(
                        prepared.manifest.pin_payload(&mid),
                    ))
                }
                None => Some(store::IssueSelection::Disabled {
                    task: id.clone(),
                    reason: "the assigned project is not opted in to agent delivery".to_string(),
                }),
            }
        };
        // CAD-1586: a dispatch is a master-target send too — the same
        // configuration-unlaunchable refusal, before the enqueue.
        // A retry of an ID that already names a row is decided by the
        // store's exact-envelope comparison; the refusal is for new sends.
        if crate::master::is_master(&alias) && !already_accepted {
            self.master_send_preflight(&alias)?;
        }
        let gated = (matches!(&caller, AgentCaller::Agent(_)) || automatic)
            && self.unattended_issue_bound(&id)?;
        // Grant→tracker order; keep canonical semantics and owner stable
        // through enqueue. Resolve blocking reads/Git before the live helper,
        // whose final expiry check must follow all dispatch setup waits.
        let _tracker = gated.then(|| pm.lock()).transpose()?;
        let mut authorized_until = None;
        if gated {
            let (live_front, _) = issue::write::load_front(&dir)?;
            let actual = issue::project::repo_identity(&worktree);
            let authority = self.unattended_dispatch_authority(&id)?.ok_or_else(|| {
                Error::rejected(
                    "the unattended run's live authority vanished during dispatch — refused",
                )
            })?;
            if live_front.owner.as_deref() != Some(alias.as_str()) {
                return Err(Error::rejected(
                    "an unattended kickoff must target the workflow's authorized worker",
                ));
            }
            let actual = actual.ok_or_else(|| {
                Error::rejected("the unattended worktree has no proven repository identity")
            })?;
            if actual.0.as_path() != Path::new(&authority.checkout) {
                return Err(Error::rejected(
                    "the unattended worktree is outside the workflow's authorized repository",
                ));
            }
            authorized_until = Some(authority.expires_at);
        }
        let (duplicate, state) = self.store.enqueue_steered_for_with_conversation(
            &alias,
            None,
            text,
            Some(reply_to),
            &mid,
            "dispatch",
            None,
            Some(&id),
            Some(&worktree.to_string_lossy()),
            &sender,
            &store::Steer::NONE,
            None,
            None,
            None,
            None,
            None,
            None,
            authorized_until,
            selection,
            None,
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
    pub(super) fn dispatch_lane(
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

/// `issue_kickoff` (CAD-606): the operator joins one worker into
/// `group` and dispatches `issue` to it. Operator-only by the
/// connection ([`Shared::operator_connection`]) — an agent, its
/// detached child, and any identity-shaped request field are refused
/// before a lane is created. The join is [`Shared::rpc_register`] (the
/// daemon half of `cadence join … --detach`) with `cwd` set to the
/// worktree [`issue::start::run`] just created; the send is
/// [`issue::dispatch::run`], the same path `cadence dispatch` uses.
/// A second call, including one racing the first, finds that lane and
/// returns it instead of opening another.
const KICKOFF_FIELDS: &[&str] = &[
    "issue", "group", "provider", "model", "effort", "alias", "note",
];

fn kickoff_text<'a>(params: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(Error::rejected(format!(
            "issue kickoff: `{key}` must be a string"
        ))),
    }
}

fn clean_kickoff_note(note: &str) -> Result<String> {
    let note = note.trim();
    if note.is_empty() {
        return Err(Error::rejected("issue kickoff: note is empty"));
    }
    if note.len() > 500 || note.chars().any(char::is_control) {
        return Err(Error::rejected(
            "issue kickoff: note must be one line of at most 500 bytes",
        ));
    }
    Ok(note.to_string())
}

impl Shared {
    /// Admit `issue_kickoff`. `alias` is the worker to join, not the
    /// caller, so it is lifted out of the operator-field refusal; every
    /// other identity-shaped field still refuses.
    fn admit_kickoff(&self, params: &Value, peer_pid: u32) -> Result<()> {
        const VERB: &str = "issue kickoff";
        if !params.is_object() {
            return Err(Error::rejected(
                "issue kickoff: params must be a JSON object",
            ));
        }
        let mut gated = params.clone();
        if let Some(obj) = gated.as_object_mut() {
            obj.remove("alias");
        }
        self.operator_connection(VERB, &gated, peer_pid)?;
        reject_identity_fields(&gated, VERB)?;
        if let Some(key) = params
            .as_object()
            .and_then(|o| o.keys().find(|k| !KICKOFF_FIELDS.contains(&k.as_str())))
        {
            return Err(Error::rejected(format!(
                "issue kickoff: unknown field '{key}'"
            )));
        }
        Ok(())
    }

    /// Provider, model and effort against the registry and the
    /// per-provider policy. Pi models go through `pi_policy` for the
    /// worker role; every other provider uses the validators its
    /// launch already runs.
    pub(super) fn kickoff_launch_policy(
        &self,
        provider: &str,
        kind: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<()> {
        let spec = crate::adapter::registry::spec(provider, kind)?;
        if let Some(model) = model {
            if !spec.launch_params.contains(&"model") {
                return Err(Error::rejected(format!(
                    "issue kickoff: provider '{provider}' does not take a model"
                )));
            }
            crate::model_defaults::validate_model_id(model)?;
            if crate::adapter::registry::pi_family_managed(provider, kind) {
                let policy = self.stored_pi_policy()?;
                crate::pi_policy::require_allowed(policy.as_ref(), "worker", model)?;
            }
        }
        if let Some(effort) = effort {
            if !spec.launch_params.contains(&"effort") {
                return Err(Error::rejected(format!(
                    "issue kickoff: provider '{provider}' does not take an effort"
                )));
            }
            match provider {
                "claude" => crate::adapter::registry::claude_effort(effort)?,
                "codex" => crate::adapter::registry::codex_effort(effort)?,
                _ if crate::adapter::registry::pi_family_managed(provider, kind) => {
                    crate::adapter::registry::pi_effort(effort)?
                }
                other => {
                    return Err(Error::rejected(format!(
                        "issue kickoff: provider '{other}' has no effort vocabulary"
                    )))
                }
            }
        }
        Ok(())
    }

    /// The issue's one verified open lane, or `None` when it has none.
    /// Several verified lanes stay an error — picking one would be a guess.
    fn kickoff_existing_lane(
        &self,
        project: &issue::project::Project,
        front: &issue::model::Front,
    ) -> Result<Option<PathBuf>> {
        match self.dispatch_lane("issue kickoff", project, front) {
            Ok(path) => Ok(Some(path)),
            Err(e) if e.to_string().contains("no open lane") => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn kickoff_lane_receipt(&self, front: &issue::model::Front, worktree: &Path) -> Value {
        let branch = front.refs.iter().find_map(|r| {
            (r.kind == "branch" && r.closed != Some(true))
                .then(|| r.path.clone())
                .flatten()
        });
        json!({
            "issue": front.id,
            "worktree": worktree,
            "branch": branch,
            "dispatched": false,
            "created": false,
            "duplicate": true,
            "duplicate_kind": "lane",
        })
    }

    /// `issue_kickoff` target decision. `true` registers the alias;
    /// `false` dispatches to the existing seat unchanged. The seat keeps
    /// its provider, model and effort — the request only names it.
    fn kickoff_register(
        existing: Option<(&str, &str)>,
        provider: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<bool> {
        let Some((seat_provider, seat_state)) = existing else {
            return Ok(true);
        };
        if seat_provider != provider {
            return Err(Error::rejected(format!(
                "issue kickoff: the seat runs {seat_provider}, not {provider} — \
                 a seat keeps its provider; change it on the seat"
            )));
        }
        if model.is_some() || effort.is_some() {
            return Err(Error::rejected(
                "issue kickoff: an existing seat keeps its model and effort; \
                 change them on the seat",
            ));
        }
        if seat_state == "attention" {
            return Err(Error::rejected(
                "issue kickoff: the seat is fenced — reconcile it before dispatching",
            ));
        }
        Ok(false)
    }

    /// `issue_kickoff` — `{issue, group, provider, model?, effort?, alias?, note?}`.
    pub(super) fn rpc_issue_kickoff(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.admit_kickoff(params, peer_pid)?;
        let id = issue::model::check_id(required_str(params, "issue")?)?;
        let group = required_str(params, "group")?;
        claim::check_alias(group, "group")?;
        let provider = required_str(params, "provider")?;
        let kind = crate::adapter::registry::default_kind(provider)?;
        let model = kickoff_text(params, "model")?;
        let effort = kickoff_text(params, "effort")?;
        let alias_arg = kickoff_text(params, "alias")?;
        if let Some(alias) = alias_arg {
            claim::check_alias(alias, "alias")?;
            // Before the lock and before `issue start`. `rpc_register`
            // is in-process, so `master_policy` never sees this alias.
            crate::master::refuse_reserved_alias(alias)?;
            if alias == group {
                return Err(Error::rejected(
                    "issue kickoff: the worker alias cannot be the group itself",
                ));
            }
        }
        let note = kickoff_text(params, "note")?
            .map(clean_kickoff_note)
            .transpose()?;
        self.kickoff_launch_policy(provider, kind, model, effort)?;
        if self.store.agent_opt(group)?.is_none() {
            return Err(Error::rejected(format!(
                "Unknown group '{group}' — no such agent"
            )));
        }

        // One kickoff at a time, so two callers share one lane.
        let _serial = self.dispatch_lock.lock().unwrap_or_else(|e| e.into_inner());
        let pm = self.pm()?;
        // One shared pre-dispatch gate (CLI dispatch uses the same
        // function); the board is the caller that requires acceptance.
        let gate = issue::dispatch::gate_issue(&pm, &id, true)?;
        let (project, front) = (gate.project, gate.front);
        if let Some(path) = self.kickoff_existing_lane(&project, &front)? {
            return Ok(self.kickoff_lane_receipt(&front, &path));
        }

        let alias = match alias_arg {
            Some(alias) => alias.to_string(),
            None => {
                let mut picked = String::new();
                for _ in 0..8 {
                    let n = &Uuid::new_v4().simple().to_string()[..6];
                    let cand = format!("{provider}-{n}");
                    if self.store.agent_opt(&cand)?.is_none() {
                        picked = cand;
                        break;
                    }
                }
                if picked.is_empty() {
                    return Err(Error::rejected(
                        "issue kickoff: could not mint a free worker alias",
                    ));
                }
                picked
            }
        };
        // One target decision: an existing row is a seat and is
        // dispatched to as-is; a free alias is registered.
        let existing = self.store.agent_opt(&alias)?;
        let register = Self::kickoff_register(
            existing
                .as_ref()
                .map(|a| (a.provider.as_str(), a.state.as_str())),
            provider,
            model,
            effort,
        )?;
        if register {
            // A minted alias is checked here too — still before `issue
            // start`, so a reserved name never opens a worktree.
            crate::master::refuse_reserved_alias(&alias)?;
        }

        // `issue start` creates a lane before `rpc_register` runs. Apply
        // the same effective-model gate here so a refused configuration
        // leaves no worktree or issue side effect behind. A seat is
        // already registered and keeps its model.
        if register && crate::adapter::registry::pi_family_managed(provider, kind) {
            let pm_dir = self.pm_dir()?;
            let confined = crate::doctor::host::read_host_overrides(&pm_dir)
                .map_err(Error::rejected)?
                .and_then(|o| o.confine_pi_workers)
                == Some(true);
            if confined {
                let defaults = self.store.model_defaults()?;
                let requested = model.map(|m| json!({"model": m}).to_string());
                let selected =
                    crate::model_defaults::resolve(crate::model_defaults::ResolveRequest {
                        provider,
                        endpoint_kind: kind,
                        runtime_role: "worker",
                        team_role: None,
                        model_policy: None,
                        params: requested.as_deref(),
                        config: &defaults.config,
                        revision: defaults.revision,
                    })?;
                let effective = selected
                    .model_selection
                    .as_ref()
                    .and_then(|s| s.get("model"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let effective = match effective {
                    Some(m) => m,
                    None => {
                        let policy = self.stored_pi_policy()?;
                        crate::pi_policy::resolve_model(policy.as_ref(), "worker", None)?
                    }
                };
                crate::adapter::pi::refuse_confined_devin(&alias, true, &effective)?;
            }
        }
        // A seat is checked before `issue start`, so a bad pane or a
        // confined worker refuses with no lane or claim left behind.
        if !register {
            issue::dispatch::check_seat(&project, &alias, false, &self.state_dir)?;
        }
        let start_args = issue::start::StartArgs {
            repo: None,
            name: None,
            base: None,
            owner: Some(alias.clone()),
            job: None,
            by: Some(group.to_string()),
            take_over: None,
        };
        let started = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            issue::start::run(&pm, &id, &start_args, "operator", &self.state_dir)
        })?;
        let worktree = started["worktree"]
            .as_str()
            .ok_or_else(|| Error::internal("issue start returned no worktree"))?;
        let mut launch = serde_json::Map::new();
        launch.insert("upstream".to_string(), json!(group));
        if let Some(model) = model {
            launch.insert("model".to_string(), json!(model));
        }
        if let Some(effort) = effort {
            launch.insert("effort".to_string(), json!(effort));
        }
        let launch_text = Value::Object(launch).to_string();
        if register {
            self.rpc_register(
                &json!({
                    "alias": alias,
                    "provider": provider,
                    "endpoint_kind": kind,
                    "role": "worker",
                    "cwd": worktree,
                    "params": launch_text,
                }),
                peer_pid,
            )?;
        }
        let args = issue::dispatch::DispatchArgs {
            to: alias.clone(),
            note: None,
            name: None,
            base: None,
            repo: None,
            reply_to: Some(group.to_string()),
            summary: None,
            job_spec: None,
            no_lessons: false,
            force: false,
            take_over: None,
        };
        let mut out = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            issue::dispatch::run(
                &pm,
                &id,
                &args,
                "operator",
                &self.state_dir,
                Some(group),
                issue::dispatch::DispatchOrigin::Explicit,
            )
        })?;
        if let Some(note) = &note {
            // A comment on the ticket — never the file the kickoff
            // tells the worker to read. That stays `issue.md`.
            issue::write::add_comment(
                &pm,
                &id,
                note,
                Some("operator"),
                Some("note"),
                None,
                "operator",
            )?;
            out["operator_note"] = json!(note);
        }
        out["provider"] = json!(provider);
        out["group"] = json!(group);
        Ok(out)
    }

    /// `issue_kickoff_options` — the form catalog for one issue.
    /// Operator-only, same connection gate as the kickoff itself.
    pub(super) fn rpc_issue_kickoff_options(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        const VERB: &str = "issue kickoff";
        self.operator_connection(VERB, params, peer_pid)?;
        reject_identity_fields(params, VERB)?;
        if let Some(key) = params
            .as_object()
            .and_then(|o| o.keys().find(|k| k.as_str() != "issue"))
        {
            return Err(Error::rejected(format!(
                "issue kickoff options: unknown field '{key}'"
            )));
        }
        let id = issue::model::check_id(required_str(params, "issue")?)?;
        let pm = self.pm()?;
        let (project, dir) = issue::write::issue_dir(&pm, &id)?;
        let (front, body) = issue::write::load_front(&dir)?;
        let items = issue::parse::acceptance_items(&body);
        let lane = self.kickoff_existing_lane(&project, &front)?;
        let policy = self.stored_pi_policy()?;
        let mut providers = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for spec in crate::adapter::registry::SPECS {
            if !spec.launch_default || spec.internal || seen.contains(&spec.provider) {
                continue;
            }
            seen.push(spec.provider);
            let takes_model = spec.launch_params.contains(&"model");
            let takes_effort = spec.launch_params.contains(&"effort");
            let models =
                if crate::adapter::registry::pi_family_managed(spec.provider, spec.endpoint_kind) {
                    json!(policy
                        .as_ref()
                        .map(|p| p.models.allow_for("worker"))
                        .unwrap_or(&[]))
                } else if takes_model {
                    Value::Null
                } else {
                    json!([])
                };
            let efforts: &[&str] = if !takes_effort {
                &[]
            } else if crate::adapter::registry::pi_family_managed(spec.provider, spec.endpoint_kind)
            {
                crate::adapter::registry::PI_EFFORTS
            } else {
                match spec.provider {
                    "claude" => crate::adapter::registry::CLAUDE_EFFORTS,
                    "codex" => crate::adapter::registry::CODEX_EFFORTS,
                    _ => &[],
                }
            };
            providers.push(json!({
                "id": spec.provider,
                "model": takes_model,
                "effort": takes_effort,
                "models": models,
                "efforts": efforts,
            }));
        }
        let mut groups: Vec<String> = self
            .store
            .agents()?
            .into_iter()
            .filter(|a| super::agent_upstream(a).is_none())
            .map(|a| a.alias)
            .collect();
        groups.sort();
        let default_model = policy
            .as_ref()
            .and_then(|p| p.models.default.for_role("worker"))
            .map(str::to_string);
        let default_provider = default_model.as_ref().map(|_| "pi");
        Ok(json!({
            "issue": id,
            "acceptance": { "items": items.len(), "ready": !items.is_empty() },
            "lane": lane.as_ref().map(|p| self.kickoff_lane_receipt(&front, p)),
            "providers": providers,
            "groups": groups,
            "defaults": {
                "group": groups.first(),
                "provider": default_provider,
                "model": default_model,
                "effort": Value::Null,
            },
        }))
    }
}

#[cfg(test)]
mod kickoff_target_tests {
    use super::*;

    #[test]
    fn free_alias_registers_and_seat_dispatches() {
        assert!(Shared::kickoff_register(None, "pi", None, None).unwrap());
        assert!(!Shared::kickoff_register(Some(("pi", "idle")), "pi", None, None).unwrap());
    }

    #[test]
    fn seat_keeps_provider_model_effort_and_is_refused_when_fenced() {
        assert!(Shared::kickoff_register(Some(("devin", "idle")), "pi", None, None).is_err());
        assert!(Shared::kickoff_register(Some(("pi", "idle")), "pi", Some("m"), None).is_err());
        assert!(Shared::kickoff_register(Some(("pi", "idle")), "pi", None, Some("high")).is_err());
        assert!(Shared::kickoff_register(Some(("pi", "attention")), "pi", None, None).is_err());
    }
}
