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
        let changed =
            self.store
                .record_designation(alias, project, active, source, APPROVAL_RECORDED_VIA)?;
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
        const VERB: &str = "delegated approve";
        let ensure = |ok: bool, why: String| {
            ok.then_some(())
                .ok_or_else(|| Error::rejected(format!("{VERB}: {why}")))
        };
        reject_identity_fields(params, VERB)?;
        reject_operator_fields(VERB, params)?;
        for field in ["approver", "identity", "verdicts", "kind"] {
            ensure(
                params.get(field).is_none(),
                format!("request field '{field}' is not accepted — the daemon derives it"),
            )?;
        }
        let alias = match self.connection_caller(peer_pid)? {
            caller_rule::Who::Agent(alias) => alias,
            caller_rule::Who::Operator => Err(Error::rejected(format!(
                "{VERB} is a designated agent's act, run from its own pane — the operator \
                 records an operator approval with `cadence audit approve`"
            )))?,
            caller_rule::Who::Unproven(why) => Err(Error::rejected(format!(
                "{VERB}: this connection derives no agent identity: {why}"
            )))?,
        };
        let pr = params.get("pr").and_then(Value::as_u64);
        let pr = pr.ok_or_else(|| Error::rejected("Missing or non-numeric 'pr'"))?;
        let (head, repo) = (required_str(params, "head")?, required_str(params, "repo")?);
        let source = required_str(params, "source")?;
        let scope_id = optional_str(params, "scope");
        // Every GitHub fact comes from the daemon's own boot-fixed `gh`.
        let gh_bin = self.delivery_gh.to_string_lossy().to_string();
        let number = pr.to_string();
        let fields = "headRefOid,state,title,author,statusCheckRollup,files,changedFiles";
        let view = crate::delivery::gh(
            &gh_bin,
            &["pr", "view", &number, "-R", repo, "--json", fields],
        )?;
        let view: Value = serde_json::from_str(&view)
            .map_err(|e| Error::rejected(format!("gh pr view: unreadable ({e})")))?;
        let live = view["headRefOid"].as_str().unwrap_or_default();
        ensure(
            live == head,
            format!("PR #{pr} head is {live}, not {head} — an approval binds the exact head"),
        )?;
        ensure(view["state"] == "OPEN", format!("PR #{pr} is not open"))?;
        let rollup = view["statusCheckRollup"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let green = !rollup.is_empty() && crate::overview::checks_green_pub(&rollup);
        ensure(green, format!("CI is not green on {head}"))?;
        let files: Vec<String> = view["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x["path"].as_str().map(str::to_string))
            .collect();
        let all = view["changedFiles"].as_u64() == Some(files.len() as u64);
        ensure(
            all,
            "gh did not list every changed file — every path must be checked".into(),
        )?;
        let title = view["title"].as_str().unwrap_or_default();
        let issue_id = crate::delegation::title_issue(title);
        let issue_id = issue_id
            .ok_or_else(|| Error::rejected(format!("{VERB}: PR #{pr}'s title names no ticket")))?;
        let pm = self.pm()?;
        let issue = crate::issue::board::find_issue(&pm.dir, &issue_id)?;
        // A designation counts only for a registration no newer than it.
        let registered = self.store.agent_opt(&alias)?.map(|a| a.created);
        let designated = self.store.designations()?.iter().any(|d| {
            d["alias"] == alias.as_str()
                && d["project"] == issue.project.as_str()
                && d["active"] == true
                && registered.is_some_and(|c| d["at"].as_f64().is_some_and(|at| c <= at))
        });
        let p = &issue.project;
        ensure(
            designated,
            format!(
                "agent '{alias}' is not designated for project '{p}' — the \
            operator runs `cadence audit designate {alias} --project {p}`"
            ),
        )?;
        let mut authors = std::collections::BTreeSet::new();
        authors.extend(issue.front.owner.clone());
        authors.extend(
            crate::delivery::load(&self.state_dir)?
                .get(&issue_id)
                .map(|r| r.worker.clone()),
        );
        authors.extend(view["author"]["login"].as_str().map(str::to_string));
        ensure(
            !authors.contains(&alias),
            format!("'{alias}' is an author of {issue_id} — an author never approves"),
        )?;
        let diff = crate::delivery::gh(&gh_bin, &["pr", "diff", &number, "-R", repo])?;
        let hits = crate::delegation::RiskPaths::load().hits(&files, &diff);
        if let Some(h) = hits.iter().find(|h| h.hard()) {
            ensure(
                false,
                format!(
                    "the diff touches trigger {} ({}) — never delegable; the operator approves",
                    h.trigger, h.what
                ),
            )?;
        }
        if let Some(id) = scope_id {
            let digest = crate::delegation::scope_digest(&issue.body);
            let live = self.store.scope_approval(id)? == Some((issue_id.clone(), digest));
            ensure(
                live,
                format!("'{id}' is no live scope pre-approval of {issue_id} as its text reads now"),
            )?;
        } else if let Some(h) = hits.first() {
            ensure(
                false,
                format!(
                    "the diff touches trigger {} ({}) — delegable only under a scope \
                pre-approval the operator recorded (--scope)",
                    h.trigger, h.what
                ),
            )?;
        }
        let notes_dir = pm.config.notes_dir();
        let notes = crate::audit::note_index(&notes_dir).ok_or_else(|| {
            Error::rejected(format!(
                "{VERB}: notes dir {} unreadable",
                notes_dir.display()
            ))
        })?;
        let mut excluded = authors;
        excluded.insert(alias.clone());
        let chosen =
            crate::delegation::pick_verdicts(&notes, &issue_id, pr, head, &excluded, scope_id)?;
        let verdicts: Vec<String> = chosen
            .iter()
            .map(|n| n.path.display().to_string())
            .collect();
        let approver = format!("delegated:{alias}");
        let approval = store::NewApproval {
            id: None,
            source,
            action: crate::delegation::DELEGATED_ACTION,
            head_sha: head,
            repo,
            pr,
        };
        let (new, id) = self.store.record_delegated(&store::NewDelegated {
            approval,
            approver: &approver,
            verdicts: &verdicts,
            scope_approval: scope_id,
        })?;
        Ok(
            json!({"state": "recorded", "duplicate": !new, "approval_id": id,
                  "approver": approver, "issue": issue_id, "verdicts": verdicts,
                  "scope_approval": scope_id, "head_sha": head,
                  "scope": {"repo": repo, "pr": pr}, "recorded_via": approver}),
        )
    }
}
