//! CAD-534: `cadence daemon` approvals RPC handlers — moved verbatim from src/daemon.rs.

use super::*;

impl Shared {
    /// `rollout_grant` (CAD-384) — the operator lets `agent` claim the
    /// rollout lease, and so stop this daemon from its own pane while it
    /// holds it. Operator only, by the connection; `until_secs` bounds
    /// the grant. Recorded as a `rollout_grant` event.
    pub(super) fn rpc_rollout_grant(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("rollout grant", params, peer_pid)?;
        let agent = required_str(params, "agent")?;
        let until = optional_u64(params, "until_secs").map(|secs| epoch_secs() + secs as f64);
        crate::rollout::grant(&self.state_dir, agent, until, "operator")
    }

    /// `rollout_revoke` (CAD-384) — end `agent`'s grant. Operator only.
    pub(super) fn rpc_rollout_revoke(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("rollout revoke", params, peer_pid)?;
        crate::rollout::revoke(&self.state_dir, required_str(params, "agent")?, "operator")
    }

    /// `staging_register` (CAD-1024) — mark this state dir as staging so a
    /// grant may name it. Operator only: `operator_connection` (positive
    /// `operator_proof` — an agent, its detached child, or a forged
    /// `--as operator:*` connection is refused). The production allowlist
    /// (`rollout::staging_register`) runs after.
    pub(super) fn rpc_staging_register(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("staging register", params, peer_pid)?;
        let port = params
            .get("board_port")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::rejected("Missing or non-numeric 'board_port'"))?;
        let port =
            u16::try_from(port).map_err(|_| Error::rejected("'board_port' is out of range"))?;
        crate::rollout::staging_register(&self.state_dir, port, "operator")
    }

    /// `staging_delegate` (CAD-1024) — the operator grants `alias` the listed
    /// ops on this staging state dir for `ttl_secs`. Operator only; the dir
    /// must be registered staging. Admits nothing — PR-3 adds the caller.
    pub(super) fn rpc_staging_delegate(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("staging delegate", params, peer_pid)?;
        // `alias` is a reserved connection-bound field (OPERATOR_FIELDS) —
        // the grantee arrives as `agent`, never `alias`.
        let alias = required_str(params, "agent")?;
        let ops = optional_strs(params, "ops")?;
        let ttl_secs = params
            .get("ttl_secs")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::rejected("Missing or non-numeric 'ttl_secs'"))?;
        crate::rollout::staging_delegate(
            &self.state_dir,
            alias,
            &ops,
            std::time::Duration::from_secs(ttl_secs),
            "operator",
        )
    }

    /// `staging_revoke` (CAD-1024) — end `alias`'s live grant here.
    /// Operator only.
    pub(super) fn rpc_staging_revoke(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("staging revoke", params, peer_pid)?;
        crate::rollout::staging_revoke(&self.state_dir, required_str(params, "agent")?, "operator")
    }

    /// `staging_delegations` (CAD-1024) — the live grants here. Read-only.
    pub(super) fn rpc_staging_delegations(&self) -> Result<Value> {
        crate::rollout::staging_delegations(&self.state_dir)
    }

    /// `approval_record` — persist an operator's merge approval for one
    /// exact head as audit evidence (`id` optional: the store picks a
    /// fresh default, see `Store::record_approval`). It grants nothing: dispatch and
    /// merge never read it; `cadence audit` binds it to the landed head.
    pub(super) fn rpc_approval_record(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("approval record", params, peer_pid)?;
        // CAD-918: delegated and scope records have their own verbs —
        // an operator-recorded `delegated-merge` would be a forgery.
        let action = optional_str(params, "action").unwrap_or("merge");
        if [
            crate::delegation::DELEGATED_ACTION,
            crate::delegation::SCOPE_ACTION,
        ]
        .contains(&action)
        {
            return Err(Error::rejected(format!(
                "approval action '{action}' is recorded only by its own verb \
                 (`audit approve --delegated` / `audit approve --action scope --issue`)"
            )));
        }
        let pr = params
            .get("pr")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::rejected("Missing or non-numeric 'pr'"))?;
        let approval = store::NewApproval {
            id: optional_str(params, "id"),
            source: required_str(params, "source")?,
            action: optional_str(params, "action").unwrap_or("merge"),
            head_sha: required_str(params, "head")?,
            repo: required_str(params, "repo")?,
            pr,
        };
        let (new, id) = self
            .store
            .record_approval(&approval, APPROVAL_RECORDED_VIA)?;
        Ok(json!({
            "state": "recorded",
            "duplicate": !new,
            "approval_id": id,
            "source": approval.source,
            "action": approval.action,
            "head_sha": approval.head_sha,
            "scope": {"repo": approval.repo, "pr": approval.pr},
            "recorded_via": APPROVAL_RECORDED_VIA,
        }))
    }

    /// `approval_record_shown` (CAD-1218) — the board's merge approval for
    /// the PR head the operator was shown. Operator connection only (the
    /// board relays over its own proven connection); `request_actor` is
    /// attribution and must pass the approver allowlist: `operator (ui)`
    /// (a loopback board session) or `<login> (tailscale)` for a login in
    /// `pm.yaml` `approvals.tailnet_logins`, read fresh on every call. The
    /// repo must belong to a registered project, the daemon's own `gh`
    /// must still show `head` as the open PR's head (`head_moved`), and a
    /// head with any earlier record, standing or revoked, is refused. The
    /// action is always `merge`; a refusal writes nothing.
    pub(super) fn rpc_approval_record_shown(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        const VERB: &str = "approval record shown";
        self.operator_connection(VERB, params, peer_pid)?;
        let deny = |why: String| Error::rejected(format!("{VERB}: {why}"));
        for field in ["source", "action", "id", "delegated", "by", "recorded_via"] {
            if params.get(field).is_some() {
                return Err(deny(format!(
                    "request field '{field}' is not accepted — the daemon derives it"
                )));
            }
        }
        let actor = request_actor(params)?;
        let pr = params
            .get("pr")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .ok_or_else(|| Error::rejected("Missing or non-numeric 'pr'"))?;
        let head = required_str(params, "head")?;
        if head.len() != 40
            || !head
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(deny(
                "head must be the full 40-character lowercase hexadecimal SHA".into(),
            ));
        }
        let raw = required_str(params, "repo")?;
        let repo = crate::delegation::repo_slug(raw)
            .ok_or_else(|| deny(format!("repo '{raw}' is not a plain owner/name")))?;
        let pm = self.pm()?;
        let known = crate::issue::project::list(&pm.dir)?
            .iter()
            .any(|p| project_repos(&pm.dir, &p.key).contains(&repo));
        if !known {
            return Err(deny(format!("no registered project has the repo {repo}")));
        }
        let allowed = match actor.strip_suffix(" (tailscale)") {
            Some(login) => pm
                .config
                .approvals
                .tailnet_logins
                .iter()
                .any(|l| l == login),
            None => actor == crate::ui::UI_ACTOR,
        };
        if !allowed {
            return Err(deny(format!(
                "'{actor}' is not on the approval allowlist — a remote board approves only for \
                 a login listed in pm.yaml approvals.tailnet_logins"
            )));
        }
        let gh_bin = self.delivery_gh.to_string_lossy().to_string();
        let view = crate::delivery::pr_view(&gh_bin, &repo, pr)
            .map_err(|e| deny(format!("reading the PR failed, nothing was approved — {e}")))?;
        let live = view["headRefOid"].as_str().unwrap_or_default();
        if live != head || view["state"] != "OPEN" {
            return Err(Error::invalid(
                "head_moved",
                format!(
                    "the PR head moved or the PR is no longer open (shown {}…, now {}…) — \
                     re-review before approving",
                    &head[..12],
                    live.chars().take(12).collect::<String>()
                ),
            ));
        }
        let source = format!("{actor} via board");
        let approval = store::NewApproval {
            id: None,
            source: &source,
            action: "merge",
            head_sha: head,
            repo: &repo,
            pr,
        };
        let id = self
            .store
            .record_approval_once(&approval, APPROVAL_RECORDED_VIA)?;
        Ok(json!({
            "state": "recorded",
            "approval_id": id,
            "source": source,
            "action": "merge",
            "head_sha": head,
            "scope": {"repo": repo, "pr": pr},
            "recorded_via": APPROVAL_RECORDED_VIA,
        }))
    }

    /// `approval_revoke` — the only way an approval is withdrawn. A
    /// cancelled or superseded message never reaches this.
    pub(super) fn rpc_approval_revoke(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("approval revoke", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let source = required_str(params, "source")?;
        let reason = required_str(params, "reason")?;
        let new = self
            .store
            .revoke_approval(id, source, reason, APPROVAL_RECORDED_VIA)?;
        Ok(json!({
            "state": "revoked",
            "duplicate": !new,
            "approval_id": id,
            "source": source,
            "reason": reason,
            "recorded_via": APPROVAL_RECORDED_VIA,
        }))
    }

    /// `approval_designate` (CAD-918) — the operator designates `alias`
    /// (or, with `active: false`, undesignates it) to record delegated
    /// approvals for `project`. Operator only, by the connection.
    pub(super) fn rpc_approval_designate(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection_on_agent("approval designate", params, peer_pid)?;
        let alias = required_str(params, "alias")?;
        let project = required_str(params, "project")?;
        let active = params
            .get("active")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let known = crate::issue::project::list(&self.pm_dir()?)?
            .iter()
            .any(|p| p.key == project);
        if !known {
            return Err(Error::rejected(format!("Unknown project '{project}'")));
        }
        let source = required_str(params, "source")?;
        let registered = self.store.agent_opt(alias)?.map(|a| a.created);
        let changed = self.store.record_designation(
            alias,
            project,
            active,
            source,
            APPROVAL_RECORDED_VIA,
            registered,
        )?;
        Ok(json!({"alias": alias, "project": project, "active": active,
                  "changed": changed, "recorded_via": APPROVAL_RECORDED_VIA}))
    }

    /// `approval_designations` — the designations in force. Read only.
    pub(super) fn rpc_approval_designations(&self) -> Result<Value> {
        Ok(json!({"designations": self.store.designations()?}))
    }

    /// `approval_scope` (CAD-918) — the operator pre-approves the scope
    /// of `issue` at ticket time, bound to a digest of its body as the
    /// tracker holds it now. Operator only, by the connection.
    pub(super) fn rpc_approval_scope(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("approval scope", params, peer_pid)?;
        let id = required_str(params, "issue")?;
        let source = required_str(params, "source")?;
        let issue = crate::issue::board::find_issue(&self.pm_dir()?, id)?;
        let digest = crate::delegation::scope_digest(&issue.body);
        let (new, approval_id) =
            self.store
                .record_scope_approval(id, &digest, source, APPROVAL_RECORDED_VIA)?;
        Ok(
            json!({"state": "recorded", "duplicate": !new, "approval_id": approval_id,
                  "issue": id, "digest": digest, "recorded_via": APPROVAL_RECORDED_VIA}),
        )
    }

    /// `approval_delegate` (CAD-918) — a designated agent records a
    /// delegated approval for one exact PR head. The approver is the
    /// agent this connection derives (never a field), and every
    /// safeguard is checked here, from facts the daemon reads itself:
    /// the designation, its own boot-fixed `gh`, the tracker, and the
    /// verdict notes through `audit::parse_note`. A refusal writes nothing.
    pub(super) fn rpc_approval_delegate(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        use crate::delegation as dg;
        const VERB: &str = "delegated approve";
        let deny = |why: String| Error::rejected(format!("{VERB}: {why}"));
        reject_identity_fields(params, VERB)?;
        reject_operator_fields(VERB, params)?;
        for field in ["approver", "identity", "verdicts", "kind"] {
            if params.get(field).is_some() {
                return Err(deny(format!(
                    "request field '{field}' is not accepted — the daemon derives it"
                )));
            }
        }
        let alias = match self.connection_caller(peer_pid)? {
            caller_rule::Who::Agent(alias) => alias,
            caller_rule::Who::Operator => Err(Error::rejected(format!(
                "{VERB} is a designated agent's act, run from its own pane — the operator \
                 records an operator approval with `cadence audit approve`"
            )))?,
            caller_rule::Who::Unproven(why) => Err(deny(format!(
                "this connection derives no agent identity: {why}"
            )))?,
        };
        let pr = params.get("pr").and_then(Value::as_u64);
        let pr = pr.ok_or_else(|| Error::rejected("Missing or non-numeric 'pr'"))?;
        let head = required_str(params, "head")?;
        let source = required_str(params, "source")?;
        let scope_id = optional_str(params, "scope");
        // The repo first, before any gh call: exactly the repo of a
        // project this agent is designated for.
        let raw = required_str(params, "repo")?;
        let repo = dg::repo_slug(raw)
            .ok_or_else(|| deny(format!("repo '{raw}' is not a plain owner/name")))?;
        let pm = self.pm()?;
        let projects = self.designated_projects(&alias)?;
        let mine: Vec<&String> = projects
            .iter()
            .filter(|p| project_repos(&pm.dir, p).contains(&repo))
            .collect();
        if mine.is_empty() {
            return Err(deny(format!(
                "agent '{alias}' is not designated for a project whose repo is {repo} — the \
                 operator runs `cadence audit designate {alias} --project <key>`"
            )));
        }
        // Every GitHub fact comes from the daemon's own boot-fixed `gh`.
        let gh_bin = self.delivery_gh.to_string_lossy().to_string();
        let gh = |args: &[&str]| -> Result<Value> {
            let out = crate::delivery::gh(&gh_bin, args)?;
            serde_json::from_str(&out).map_err(|e| deny(format!("gh: unreadable ({e})")))
        };
        let number = pr.to_string();
        let fields = "headRefOid,headRefName,baseRefName,state,title,author,\
                      statusCheckRollup,files,changedFiles";
        let view = gh(&["pr", "view", &number, "-R", &repo, "--json", fields])?;
        let live = view["headRefOid"].as_str().unwrap_or_default();
        if live != head {
            return Err(deny(format!(
                "PR #{pr} head is {live}, not {head} — an approval binds the exact head"
            )));
        }
        if view["state"] != "OPEN" {
            return Err(deny(format!("PR #{pr} is not open")));
        }
        let title = view["title"].as_str().unwrap_or_default();
        let issue_id = dg::title_issue(title)
            .ok_or_else(|| deny(format!("PR #{pr}'s title names no ticket")))?;
        let issue = crate::issue::board::find_issue(&pm.dir, &issue_id)?;
        if !mine.contains(&&issue.project) {
            return Err(deny(format!(
                "agent '{alias}' is not designated for project '{}'",
                issue.project
            )));
        }
        let mut authors = std::collections::BTreeSet::new();
        authors.extend(issue.front.owner.as_deref().map(dg::alias_of));
        let loop_rec = crate::delivery::load(&self.state_dir)?;
        authors.extend(loop_rec.get(&issue_id).map(|r| r.worker.to_lowercase()));
        authors.extend(view["author"]["login"].as_str().map(str::to_lowercase));
        if authors.contains(&alias) {
            return Err(deny(format!(
                "'{alias}' is an author of {issue_id} — an author never approves"
            )));
        }
        // Every changed path, both sides of a rename, must be delegable.
        let listed: Vec<&str> = view["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x["path"].as_str())
            .collect();
        if view["changedFiles"].as_u64() != Some(listed.len() as u64) {
            return Err(deny("gh did not list every changed file".into()));
        }
        let diff = crate::delivery::gh(&gh_bin, &["pr", "diff", &number, "-R", &repo])?;
        let mut paths = dg::diff_paths(&diff)?;
        paths.extend(listed.iter().map(|p| p.to_string()));
        let rp = dg::RiskPaths::load();
        let mut schema = false;
        for path in &paths {
            match rp.classify(path) {
                dg::PathClass::Delegable => {}
                dg::PathClass::Schema => schema = true,
                dg::PathClass::Operator(t) => {
                    let why = match t.as_slice() {
                        [] => "outside the delegable allowlist".to_string(),
                        t => format!("trigger {}", t.join(", ")),
                    };
                    return Err(deny(format!("{path} needs operator ({why})")));
                }
            }
        }
        match scope_id {
            None if schema => {
                return Err(deny(
                    "a schema path needs operator, or a scope pre-approval (--scope)".into(),
                ))
            }
            None => {}
            Some(id) => {
                let digest = dg::scope_digest(&issue.body);
                if self.store.scope_approval(id)? != Some((issue_id.clone(), digest)) {
                    return Err(deny(format!(
                        "'{id}' is no live scope pre-approval of {issue_id} as its text reads now"
                    )));
                }
                let branch = view["headRefName"].as_str().unwrap_or_default();
                let lane = issue.front.refs.iter().any(|r| {
                    r.kind == "branch"
                        && r.closed != Some(true)
                        && r.path.as_deref() == Some(branch)
                });
                if !lane {
                    return Err(deny(format!(
                        "PR branch '{branch}' is not a lane branch of {issue_id}, so its scope \
                         pre-approval does not cover it"
                    )));
                }
            }
        }
        // CI: the base branch's required checks, from check runs only.
        let base = view["baseRefName"].as_str().unwrap_or_default();
        let branch = gh(&["api", &format!("repos/{repo}/branches/{base}")])?;
        let runs = gh(&[
            "api",
            &format!("repos/{repo}/commits/{head}/check-runs?filter=all&per_page=100"),
        ])?;
        let workflows = gh(&[
            "api",
            &format!("repos/{repo}/actions/runs?head_sha={head}&event=pull_request&per_page=100"),
        ])?;
        let rollup = view["statusCheckRollup"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        dg::ci_green(&branch, &runs, &workflows, (head, pr, base), &rollup)
            .map_err(|why| deny(format!("CI is not green on {head}: {why}")))?;
        let notes_dir = pm.config.notes_dir();
        let rows = crate::audit::verdicts_in(&notes_dir, &issue_id, pr, head)?;
        let mut excluded = authors;
        excluded.insert(alias.clone());
        let chosen = dg::pick_verdicts(&rows, &excluded, scope_id)?;
        let mut verdicts = Vec::new();
        let mut sha = Vec::new();
        let mut reviewers = Vec::new();
        for (path, reviewer) in chosen {
            let bytes = std::fs::read(&path).map_err(|e| deny(format!("{path}: {e}")))?;
            sha.push(dg::sha256_hex(&bytes));
            verdicts.push(path);
            reviewers.push(reviewer);
        }
        let approver = format!("delegated:{alias}");
        let approval = store::NewApproval {
            id: None,
            source,
            action: dg::DELEGATED_ACTION,
            head_sha: head,
            repo: &repo,
            pr,
        };
        let (new, id) = self.store.record_delegated(&store::NewDelegated {
            approval,
            approver: &approver,
            verdicts: &verdicts,
            verdict_sha256: &sha,
            reviewers: &reviewers,
            scope_approval: scope_id,
        })?;
        Ok(
            json!({"state": "recorded", "duplicate": !new, "approval_id": id,
                  "approver": approver, "issue": issue_id, "verdicts": verdicts,
                  "verdict_sha256": sha, "reviewers": reviewers,
                  "scope_approval": scope_id, "head_sha": head,
                  "scope": {"repo": repo, "pr": pr}, "recorded_via": approver}),
        )
    }

    /// The projects `alias` is designated for now: active, and no older
    /// than its current registration (a re-registered alias inherits
    /// nothing).
    fn designated_projects(&self, alias: &str) -> Result<Vec<String>> {
        let registered = self.store.agent_opt(alias)?.map(|a| a.created);
        Ok(self
            .store
            .designations()?
            .into_iter()
            .filter(|d| d["alias"] == alias && d["active"] == true)
            .filter(|d| registered.is_some_and(|c| d["at"].as_f64().is_some_and(|at| c <= at)))
            .filter_map(|d| d["project"].as_str().map(str::to_string))
            .collect())
    }
}

/// A project's GitHub repos as lowercase `owner/name`, from each repo's
/// `remote`, else its checkout's origin.
fn project_repos(pm_dir: &std::path::Path, key: &str) -> Vec<String> {
    let projects = crate::issue::project::list(pm_dir).unwrap_or_default();
    let Some(project) = projects.into_iter().find(|p| p.key == key) else {
        return vec![];
    };
    project
        .repos
        .iter()
        .filter_map(|r| match (&r.remote, &r.path) {
            (Some(remote), _) => crate::issue::project::normalize_remote(remote)
                .strip_prefix("github.com/")
                .map(str::to_string),
            (None, Some(path)) => {
                crate::audit::origin_slug(&crate::issue::project::expand_home(path))
            }
            _ => None,
        })
        .map(|s| s.to_ascii_lowercase())
        .collect()
}
