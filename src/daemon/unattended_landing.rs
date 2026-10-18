//! Consume an operator-issued workflow grant, never an agent's approval claim.
//! The operator-process observer calls this while holding `delivery_lock`.

use serde_json::{json, Value};

use super::delivery_requirements_rpc;
use super::unattended::AuthorizedRun;
use super::Shared;
use crate::delivery::{self, Record, State};
use crate::error::{Error, Result};
use crate::issue::task_report;

fn reject(reason: &str) -> Error {
    Error::rejected(format!("unattended landing held: {reason}"))
}

fn same_authorization(a: &AuthorizedRun, b: &AuthorizedRun) -> bool {
    a.grant_id == b.grant_id
        && a.project == b.project
        && a.epic == b.epic
        && a.repo == b.repo
        && a.paths == b.paths
        && a.expires_at == b.expires_at
}

fn check_scope(answer: &Value, run: &AuthorizedRun) -> Result<()> {
    if answer["project"] != run.project || answer["repo"] != run.repo {
        return Err(reject(
            "the pull request is outside the authorized project/repository",
        ));
    }
    let requirements = &answer["requirements"];
    if requirements["activation"] != "active"
        || !matches!(
            requirements["class"].as_str(),
            Some("routine" | "consequential")
        )
        || requirements["operator_approval"] != false
        || requirements["browser_qa"] != false
        || requirements["security_capable"] != false
        || requirements["reviews"]
            .as_u64()
            .is_none_or(|count| count > 1)
    {
        return Err(reject("current policy requires a protected, sensitive or otherwise unsupported approval/review"));
    }
    let changes = answer["changes"]
        .as_array()
        .filter(|changes| !changes.is_empty())
        .ok_or_else(|| reject("a complete nonempty changed-file list is required"))?;
    for change in changes {
        let path = change["path"]
            .as_str()
            .ok_or_else(|| reject("a changed file has no path"))?;
        if path == ".github/workflows/ci.yml" {
            return Err(reject(
                "automatic landing cannot change its trusted integration workflow",
            ));
        }
        if path.is_empty()
            || path.starts_with('/')
            || path.chars().any(char::is_control)
            || path.split('/').any(|part| matches!(part, "" | "." | ".."))
            || !run
                .paths
                .iter()
                .any(|glob| crate::review::glob_match(glob, path))
        {
            return Err(reject(
                "the actual pull-request diff exceeds the authorized file allowlist",
            ));
        }
        let regular = change["new_mode"] == "100644"
            && match change["status"].as_str() {
                Some("A") => change["old_mode"] == "000000",
                Some("M") => change["old_mode"] == "100644",
                _ => false,
            };
        if !regular {
            return Err(reject("deletions, renames, executable/type/mode changes or unknown file evidence require explicit approval"));
        }
    }
    Ok(())
}

fn check_view(view: &Value, head: &str) -> Result<()> {
    if view["headRefOid"].as_str() != Some(head) {
        return Err(Error::invalid(
            "head_moved",
            "the pull-request head moved before unattended enqueue",
        ));
    }
    if view["state"] != "OPEN" {
        return Err(reject("the pull request is no longer open"));
    }
    Ok(())
}

impl Shared {
    /// Caller holds the grant READ lock before `delivery_lock`: the grant
    /// cannot be replaced during enqueue. Persist intent BEFORE `gh`;
    /// uncertainty never authorizes a second mutation.
    /// True preserves this exact head's authorized remote request, including
    /// known pending admission; only an observed entry enters Enqueued.
    pub(super) fn try_unattended_enqueue(&self, rec: &mut Record) -> Result<bool> {
        // This display marker cannot authorize reconciliation. Recompute
        // readiness and every authority check independently on each pass.
        rec.unattended_pending = false;
        if rec.state != State::Passed || !rec.merge_ready() {
            return Ok(false);
        }
        let Some(run) = self.unattended_authorization_for_issue(&rec.issue)? else {
            return Ok(false);
        };
        let verdict = rec
            .verdict
            .as_ref()
            .ok_or_else(|| reject("an independent PASS is required"))?;
        if verdict.verdict != "pass"
            || rec.head.as_deref() != Some(verdict.sha.as_str())
            || verdict.reviewer == rec.worker
            || rec.reviewer.as_deref() != Some(verdict.reviewer.as_str())
            || !self.store.unattended_review_recorded(rec)?
        {
            return Err(reject("an independent, current, native scope-bound PASS with explicit risk:auto on this exact head is required"));
        }
        let head = verdict.sha.as_str();
        // A newer owned turn may have been queued before a crash saved
        // Working. A historical PASS never authorizes work still in flight.
        match self.unattended_worker_complete(
            &rec.issue,
            &rec.worker,
            &mut rec.unattended_turn,
            head,
        ) {
            Ok(true) => {}
            Ok(false) => {
                rec.verdict = None;
                rec.enter(State::Working, crate::issue::time::now_epoch());
                rec.unattended_hold = false;
                rec.note = None;
                return Ok(false);
            }
            Err(error) => {
                rec.verdict = None;
                rec.enter(State::Escalated, crate::issue::time::now_epoch());
                return Err(error);
            }
        }
        let pr_url = rec
            .pr
            .as_deref()
            .ok_or_else(|| reject("the review has no pull request"))?;
        let (repo, number) = task_report::parse_pr_url(pr_url)?;
        if repo.to_ascii_lowercase() != run.repo || rec.project != run.project {
            return Err(reject(
                "the reviewed pull request is outside the authorized repository",
            ));
        }
        let request = json!({"issue": rec.issue, "pr": pr_url, "head": head});
        let classification = delivery_requirements_rpc::classify(self, &request)?;
        check_scope(&classification, &run)?;
        let base = classification["base"]
            .as_str()
            .ok_or_else(|| reject("current policy evidence has no base commit"))?;
        let base_ref = classification["base_ref"]
            .as_str()
            .ok_or_else(|| reject("current policy evidence has no base branch name"))?;
        if !delivery_requirements_rpc::full_ci_green(
            self, &run.repo, number, head, base, base_ref, true,
        )? {
            if let Some(observed) = rec.observed.as_mut() {
                observed.ci_green = false;
            }
            if rec.unattended_hold {
                rec.unattended_hold = false;
                rec.note = None;
            }
            return Ok(false);
        }
        let confirmed = delivery_requirements_rpc::classify(self, &request)?;
        if confirmed != classification {
            return Err(reject(
                "the head, base, file scope or trusted policy changed during authorization",
            ));
        }
        if !self.store.unattended_review_recorded(rec)? {
            return Err(reject("the native review changed during authorization"));
        }
        let gh = self.delivery_gh.to_string_lossy();
        let view = delivery::pr_view(&gh, &repo, number)?;
        check_view(&view, head)?;
        let mut queue_entry =
            delivery_requirements_rpc::unattended_queue_entry(self, &repo, number, head)?;
        let auto_merge = !view
            .get("autoMergeRequest")
            .ok_or_else(|| reject("the fresh PR view has no queue-state evidence"))?
            .is_null();
        // The protected observer's snapshot may predate this fresh read.
        // Do not hide an unbound live request behind stale auto_merge=false.
        if let Some(observed) = rec.observed.as_mut() {
            observed.auto_merge = auto_merge;
        }
        let current = self
            .unattended_authorization_for_issue(&rec.issue)?
            .ok_or_else(|| reject("the native workflow authorization disappeared"))?;
        if !same_authorization(&run, &current) {
            return Err(reject(
                "the workflow authorization changed during collection",
            ));
        }
        let intent = json!({
            "issue": rec.issue, "project": run.project, "epic": run.epic, "worker": rec.worker,
            "grant_id": run.grant_id, "grant_expires_at": run.expires_at,
            "repo": run.repo, "pr": number, "head": head, "base": base,
            "policy_digest": classification["policy_digest"], "class": classification["requirements"]["class"],
            "reviewer": verdict.reviewer, "report": verdict.report, "risk": "auto",
            "ci_authority": "enforced_merge_queue",
        });
        let existing = self.store.unattended_merge_intent(&rec.issue, head)?;
        let reconciled = if let Some(existing) = existing {
            // Renewal changes expiry, not the original enqueue's scope.
            // Keep that historical deadline; validate the current live grant
            // independently rather than replacing or replaying its intent.
            let same_scope =
                existing
                    .as_object()
                    .zip(intent.as_object())
                    .is_some_and(|(old, current)| {
                        old.len() == current.len()
                            && existing["grant_expires_at"]
                                .as_f64()
                                .is_some_and(|at| at.is_finite() && at > 0.0)
                            && current.iter().all(|(key, value)| {
                                key == "grant_expires_at" || old.get(key) == Some(value)
                            })
                    });
            if !same_scope {
                return Err(reject(
                    "a previous enqueue intent has different authority or review evidence",
                ));
            }
            if queue_entry.is_none() && !auto_merge {
                return Err(reject("a prior enqueue outcome is unresolved or the queue was ejected; operator must inspect it, no mutation was replayed"));
            }
            // The remote view proves the intended exact-head queue state;
            // it does not prove which process enabled it.
            true
        } else {
            if auto_merge || queue_entry.is_some() {
                return Err(reject(
                    "auto-merge is enabled without this grant's native enqueue intent",
                ));
            }
            if !self.store.record_unattended_merge_intent(intent)? {
                return Err(reject(
                    "an enqueue intent appeared concurrently; inspect it before retrying",
                ));
            }
            delivery::gh(&gh, &[
                "pr", "merge", &number.to_string(), "-R", &repo,
                "--auto", "--squash", "--match-head-commit", head,
            ]).map_err(|error| reject(&format!("enqueue did not return success ({error}); its durable intent is held for reconciliation, not replay")))?;
            // Successful --auto only requests admission. Preserve the intent
            // if queue admission is delayed; a later pass reconciles it.
            queue_entry =
                delivery_requirements_rpc::unattended_queue_entry(self, &repo, number, head)?;
            false
        };
        let Some(queue_entry) = queue_entry else {
            rec.unattended_pending = true;
            rec.unattended_hold = false;
            rec.note = Some(format!(
                "Authorized queue request at {head}; native admission is pending. No mutation will be replayed."
            ));
            return Ok(true);
        };
        self.store.record_unattended_merge_enqueued(json!({
            "issue": rec.issue, "head": head, "grant_id": run.grant_id,
            "reconciled": reconciled, "queue_entry": queue_entry,
        }))?;
        let note = format!(
            "Enqueued at {head} under operator-issued workflow grant {}{}.",
            run.grant_id,
            if reconciled {
                " (confirmed from the remote queue; no mutation replayed)"
            } else {
                ""
            },
        );
        rec.enter(State::Enqueued, crate::issue::time::now_epoch());
        rec.disable_auto = false;
        rec.unattended_hold = false;
        rec.note = Some(note);
        Ok(true)
    }

    /// Remote merge evidence alone cannot complete an unattended run. Expired
    /// or revoked authority holds automatic postmerge settlement for the
    /// operator rather than silently extending the grant.
    pub(super) fn unattended_merge_done_refusal(
        &self,
        rec: &Record,
        head: &str,
    ) -> Result<Option<String>> {
        let run = match self.unattended_authorization_for_issue(&rec.issue) {
            Ok(None) => return Ok(None),
            Ok(Some(run)) => run,
            Err(error) => {
                return Ok(Some(format!(
                "unattended postmerge verification held: {error}; operator verification is required"
            )))
            }
        };
        let Some(intent) = self.store.unattended_merge_intent(&rec.issue, head)? else {
            return Ok(Some(
                "the merged head has no native unattended authorization intent".to_string(),
            ));
        };
        let Some(enqueued) = self.store.unattended_merge_enqueued(&rec.issue, head)? else {
            return Ok(Some(
                "native queue admission was never observed; operator verification is required"
                    .to_string(),
            ));
        };
        let (repo, number) = task_report::parse_pr_url(rec.pr.as_deref().unwrap_or_default())?;
        let verdict = rec.verdict.as_ref();
        let matches = intent["grant_id"] == run.grant_id
            && intent["project"] == run.project
            && intent["epic"] == run.epic
            && intent["issue"] == rec.issue
            && intent["worker"] == rec.worker
            && intent["head"] == head
            && intent["repo"] == run.repo
            && run.repo == repo.to_ascii_lowercase()
            && intent["pr"] == number
            && intent["ci_authority"] == "enforced_merge_queue"
            && enqueued["grant_id"] == run.grant_id
            && enqueued["queue_entry"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
            && matches!(intent["class"].as_str(), Some("routine" | "consequential"))
            && verdict.is_some_and(|v| {
                v.verdict == "pass"
                    && v.sha == head
                    && v.reviewer != rec.worker
                    && intent["reviewer"] == v.reviewer
                    && intent["report"] == v.report
            })
            && self.store.unattended_review_recorded(rec)?;
        if matches {
            let Some(base) = intent["base"].as_str() else {
                return Ok(Some(
                    "native enqueue intent has no trusted base".to_string(),
                ));
            };
            if let Err(error) =
                delivery_requirements_rpc::unattended_queue_authority(self, &repo, base)
            {
                return Ok(Some(format!(
                    "postmerge queue enforcement is unproven: {error}"
                )));
            }
        }
        Ok((!matches).then(|| {
            "the native unattended intent does not bind this run, PR and independent exact-head review".to_string()
        }))
    }
}
