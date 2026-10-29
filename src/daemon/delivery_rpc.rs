//! The daemon side of the worker loop (CAD-431): every transition of
//! [`crate::delivery::Record`], under `delivery_lock`.
//!
//! - `master_dispatch` opens a record for the ticket it dispatched.
//! - The report router's pass ([`Shared::route_delivery`]) takes the
//!   worker's newest `done` report (`sha:` + `pr:`) and routes a review
//!   to an independent reviewer ([`crate::delivery::pick_reviewer`]) with
//!   a kickoff the daemon composes.
//! - `report_verdict` is the only way a `verdict` report is filed: the
//!   caller must be the ticket's assigned reviewer by its verified
//!   connection, and the sha the head under review. REVISE goes back to
//!   the worker, at most [`crate::delivery::MAX_REVISE`] times; PASS
//!   waits for green CI.
//! - `delivery_observe`, `delivery_merge` and `delivery_decline` are the
//!   proven operator's: GitHub facts come only from the operator's own
//!   process (`cadence delivery sync|merge`), and a head that moved after
//!   review re-enters review and asks that process to disable auto-merge.
//! - The observation that moves a record into `merged` marks the ticket
//!   `done` (CAD-449) — once, and only for the reviewed merge
//!   ([`Shared::merge_done_refusal`]); the router pass retries a
//!   tracker write that failed ([`Shared::settle_ticket_done`]).
//!
//! Every refusal happens before anything is written — the tracker, the
//! record and the message queue alike.

use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::delivery_requirements_rpc::{self, RoutineReadiness};
use super::{
    check_values, optional_str, optional_strs, request_actor, required_str, Shared,
    APPROVAL_RECORDED_VIA, DAEMON_ALIAS,
};
use crate::delivery::{self, Candidate, Observed, Record, State, TicketDone, VerdictRec};
use crate::error::{Error, Result};
use crate::issue::{self, task_report, Pm};
use crate::master;
use crate::peer::AgentCaller;
use crate::store;

/// Longest decline reason.
const REASON_MAX: usize = 2_000;

#[cfg(all(test, feature = "test-seam"))]
#[path = "cad622_acceptance.rs"]
mod cad622_acceptance;

fn now() -> i64 {
    crate::issue::time::now_epoch()
}

fn apply_routine_ready_observation(
    rec: &mut Record,
    evidence: delivery::ReadyEvidence,
    was_disable: bool,
    now: i64,
) {
    let prior_state = rec.state;
    let prior_binding = rec.ready_binding.clone();
    let prior_evidence = rec.ready_evidence.clone();
    let expected_head = evidence.head.clone();
    let binding = delivery::ReadyBinding {
        repo: evidence.repo.clone(),
        pr: evidence.pr,
        head: evidence.head.clone(),
        base: evidence.base.clone(),
        base_ref: evidence.base_ref.clone(),
        policy_digest: evidence.policy_digest.clone(),
        ci_run_id: evidence.ci_run_id,
        ci_run_url: evidence.ci_run_url.clone(),
        outcome_report: evidence.outcome_report.clone(),
    };
    let unchanged =
        prior_binding.as_ref() == Some(&binding) && prior_evidence.as_ref() == Some(&evidence);
    if prior_state == State::Enqueued && !unchanged {
        // A changed base, policy, CI run or report invalidates the
        // old enqueue intent even when HEAD did not move. Do not install
        // the changed read as replacement proof in this observation.
        rec.ready_evidence = Some(evidence);
        rec.enter(State::Ready, now);
    } else {
        rec.ready_binding = Some(binding);
        rec.ready_evidence = Some(evidence);
    }
    if prior_state == State::Ready
        && unchanged
        && !was_disable
        && rec.observed.as_ref().is_some_and(|o| {
            rec.head.as_deref() == Some(expected_head.as_str())
                && o.head == expected_head
                && o.ci_green
                && o.pr_state == "OPEN"
                && o.auto_merge
        })
    {
        // The reviewed-script enqueue is external to this daemon.
        rec.enter(State::Enqueued, now);
    }
}

#[cfg(test)]
#[path = "cad1298_observer_acceptance.rs"]
mod observer_acceptance;
#[cfg(all(test, feature = "test-seam"))]
#[path = "cad1298_security_acceptance.rs"]
mod security_acceptance;

fn hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn not_in_loop(id: &str) -> Error {
    Error::rejected(format!(
        "{id} is not in the review loop — only tickets the master dispatched are \
         (`cadence delivery ls`)"
    ))
}

impl Shared {
    /// Open the loop's record for a ticket `master_dispatch` just sent
    /// to `worker`. A finished record is replaced; a live one is kept.
    pub(super) fn delivery_start(&self, id: &str, project: &str, worker: &str) -> Result<()> {
        let dispatched_at = self
            .store
            .worker_issue_turn(id, worker)?
            .map_or_else(now, |turn| turn.created.floor() as i64);
        self.delivery_adopt(id, project, worker, dispatched_at)
    }

    pub(super) fn unattended_worker_complete(
        &self,
        issue: &str,
        worker: &str,
        marker: &mut Option<String>,
        head: &str,
    ) -> Result<bool> {
        let turn = self.store.worker_issue_turn(issue, worker)?;
        let turn = turn.ok_or_else(|| {
            Error::invalid(
            "unattended_completion_missing",
            "the done report has no native owned worker turn; operator must inspect the dispatch",
        )
        })?;
        match turn.state.as_str() {
            "queued" | "running" => Ok(false),
            "completed" if turn.sha.as_deref() == Some(head) => Ok(true),
            "completed" => {
                let error = Error::invalid(
                    "unattended_completion_revision",
                    format!(
                        "worker turn {} completed without the done report's exact revision {head}; operator must inspect its durable result",
                        turn.id
                    ),
                );
                *marker = Some(turn.id);
                Err(error)
            }
            "failed" => {
                let error = Error::invalid(
                    "unattended_completion_failed",
                    format!(
                        "worker turn {} definitively failed; inspect its durable failure, restore provider availability, resume the lane with `cadence agent resume {}` and explicitly start a new owned turn — do not relabel the failed result as completed",
                        turn.id, worker
                    ),
                );
                *marker = Some(turn.id);
                Err(error)
            }
            _ => {
                let error = Error::invalid(
                    "unattended_completion_unresolved",
                    format!(
                        "worker turn {} is {}; reconcile its owned outcome before continuing, never replay it",
                        turn.id, turn.state
                    ),
                );
                *marker = Some(turn.id);
                Err(error)
            }
        }
    }

    /// CAD-484: open the record for a lane the checkup adopted — the
    /// worker's done report predates the loop, so the dispatch bound is
    /// the report's own `at`, letting the router take it rather than
    /// filter it as filed before the record existed.
    pub(super) fn delivery_adopt(
        &self,
        id: &str,
        project: &str,
        worker: &str,
        dispatched_at: i64,
    ) -> Result<()> {
        let _g = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = delivery::load(&self.state_dir)?;
        if all.get(id).is_some_and(|r| !r.state.terminal()) {
            return Ok(());
        }
        let mut rec = Record::new(id, project, worker, now());
        rec.dispatched_at = dispatched_at;
        all.insert(id.to_string(), rec);
        delivery::save(&self.state_dir, &all)
    }

    /// One router pass over the loop (CAD-431): the worker's newest
    /// unconsumed `done` report goes to review; an unstaffed review is
    /// retried. Returns how many records moved.
    pub(super) fn route_delivery(self: &Arc<Self>) -> Result<usize> {
        if let Err(e) = self.settle_ticket_done() {
            tracing::warn!("delivery router, merged tickets: {e}");
        }
        let _grant = self
            .unattended_lock
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let _g = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = delivery::load(&self.state_dir)?;
        if all.values().all(|r| r.state.terminal()) {
            return Ok(0);
        }
        let pm = self.pm()?;
        // CAD-362: pair-precision history, snapshot before the mutable
        // pass — `pick_reviewer` reads every record for the pair.
        let history: Vec<delivery::Record> = all.values().cloned().collect();
        let hist: Vec<&delivery::Record> = history.iter().collect();
        // The PRs live records hold — one PR belongs to one ticket.
        let held: Vec<(String, String)> = all
            .values()
            .filter(|r| !r.state.terminal())
            .filter_map(|r| Some((r.issue.clone(), delivery::pr_ref(r.pr.as_deref()?)?)))
            .collect();
        let mut moved = 0;
        for rec in all.values_mut() {
            // One ticket's failure (a message the queue refuses, an
            // unreadable ticket) never holds up the others; it is
            // retried next pass.
            match self.route_record(&pm, rec, &held, &hist) {
                Ok(true) => moved += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!("delivery router, {}: {e}", rec.issue),
            }
        }
        if moved > 0 {
            delivery::save(&self.state_dir, &all)?;
            self.wake();
        }
        Ok(moved)
    }

    /// Route one record; `true` when it changed.
    fn route_record(
        self: &Arc<Self>,
        pm: &Pm,
        rec: &mut Record,
        held: &[(String, String)],
        hist: &[&Record],
    ) -> Result<bool> {
        if rec.state.terminal() {
            return Ok(false);
        }
        if rec.state == State::Escalated
            && (!rec.unattended_hold
                || !matches!(
                    self.unattended_authorization_for_issue(&rec.issue),
                    Ok(Some(_))
                ))
        {
            return Ok(false);
        }
        if rec.state == State::Escalated && rec.unattended_turn.is_some() {
            // A recorded REVISE is not permission to replay its failed or
            // unknown turn. Reconciliation may resume the existing report;
            // a new revision needs its own fresh done report below.
            if self
                .store
                .worker_issue_turn(&rec.issue, &rec.worker)?
                .is_some_and(|turn| {
                    turn.state == "completed"
                        && turn.sha.is_some()
                        && turn.sha.as_deref() == rec.head.as_deref()
                })
            {
                self.start_review(pm, rec, hist)?;
                return Ok(true);
            }
        }
        if rec.state == State::Escalated && rec.unattended_turn.is_none() {
            if let Some(verdict) = rec.verdict.as_ref().filter(|v| {
                rec.head.as_deref() == Some(v.sha.as_str())
                    && rec.reviewer.as_deref() == Some(v.reviewer.as_str())
                    && v.reviewer != rec.worker
            }) {
                if verdict.verdict == "pass" && self.store.review_export_recorded(rec)? {
                    rec.enter(State::Passed, now());
                    rec.unattended_hold = false;
                    rec.note = None;
                    return Ok(true);
                }
                if verdict.verdict == "revise"
                    && self.store.verdict_recorded(
                        &rec.issue,
                        "revise",
                        &verdict.sha,
                        &verdict.reviewer,
                        &verdict.report,
                    )?
                {
                    if rec.revisions >= delivery::MAX_REVISE {
                        rec.note = Some(format!(
                            "{} REVISE verdicts — the review did not converge; the operator decides",
                            rec.revisions
                        ));
                    } else {
                        self.queue_revision(rec, now())?;
                        rec.note = None;
                    }
                    rec.unattended_hold = false;
                    return Ok(true);
                }
            }
            let note = rec.note.take();
            if let Err(error) = self.start_review(pm, rec, hist) {
                rec.unattended_hold = true;
                rec.note = note;
                return Err(error);
            }
            return Ok(rec.state != State::Escalated || !rec.unattended_hold || rec.note != note);
        }
        let dir = pm.dir.join(&rec.project).join(&rec.issue);
        let mut fresh: Vec<Value> = task_report::list(&dir, &rec.issue)
            .into_iter()
            .filter(|r| {
                r["kind"] == "done"
                    && r["error"].is_null()
                    && r["agent"].as_str() == Some(rec.worker.as_str())
                    && r["at"]
                        .as_str()
                        .and_then(issue::time::parse_iso)
                        .is_some_and(|at| at >= rec.dispatched_at)
                    && !rec
                        .handled
                        .iter()
                        .any(|h| Some(h.as_str()) == r["name"].as_str())
            })
            .collect();
        // Newest last by filing order, not by name: a same-second report
        // (`…-w1-1.md`) sorts before the first by name, and a refused one
        // must never hide a valid one filed after it.
        fresh.sort_by_key(|r| {
            delivery::filing_order(r["name"].as_str().unwrap_or_default(), &rec.worker)
        });
        let Some(latest) = fresh.last().cloned() else {
            if rec.state == State::Unstaffed
                || (rec.state == State::Working && rec.outcome_report.is_some())
            {
                let before = rec.state;
                self.start_review(pm, rec, hist)?;
                return Ok(rec.state != before || rec.ready_evidence.is_some());
            }
            return Ok(false);
        };
        let name = latest["name"].as_str().unwrap_or_default();
        let refusal = match (latest["sha"].as_str(), latest["pr"].as_str()) {
            (Some(sha), Some(pr)) => match self.pr_refusal(pm, rec, pr, sha, held)? {
                None => {
                    self.on_done(
                        pm,
                        rec,
                        sha,
                        pr,
                        latest["path"].as_str().unwrap_or(""),
                        hist,
                    )?;
                    None
                }
                why => why,
            },
            _ => Some(
                "has no `sha:` and `pr:` — the review loop needs both. File a new done \
                 report with `sha: <head>` and `pr: https://github.com/<owner>/<repo>/pull/<n>`"
                    .to_string(),
            ),
        };
        // A refused done report changes nothing in the record: the worker
        // is told once (the message id is the report's), and the next
        // done report is judged afresh.
        if let Some(why) = refusal {
            let text = format!(
                "[review] {id}: your done report {name} {why}.",
                id = rec.issue
            );
            let mid = format!("done-refused-{}", hash(&format!("{}/{name}", rec.issue)));
            self.send_as(
                &json!({"alias": rec.worker, "text": text, "message": mid,
                        "source": "review"}),
                &|_| Ok(store::Sender::Unattributed),
            )?;
            let _ = self.store.event_public(
                DAEMON_ALIAS,
                "review_done_refused",
                json!({"issue": rec.issue, "report": name, "why": why}),
            );
            return Ok(false);
        }
        rec.handled.extend(
            fresh
                .iter()
                .filter_map(|r| r["name"].as_str().map(str::to_string)),
        );
        Ok(true)
    }

    /// Why a done report's PR cannot enter review, if it cannot: the PR
    /// must be in one of the ticket's project repos (`repos[].remote`,
    /// compared normalized), and no other live ticket may hold it —
    /// otherwise the operator's `gh` would enqueue a PR nobody
    /// dispatched. CAD-622: and an app's own team agent never authors a
    /// change to that app's source — the PR's own changed files (or its
    /// repository, when an installation pinned it as the app's git
    /// source) name the app.
    fn pr_refusal(
        &self,
        pm: &Pm,
        rec: &mut Record,
        pr: &str,
        sha: &str,
        held: &[(String, String)],
    ) -> Result<Option<String>> {
        if let Some(why) = delivery::project_pr_refusal(&pm.dir, &rec.project, pr)? {
            return Ok(Some(why));
        }
        let key = delivery::pr_ref(pr).unwrap_or_default();
        if let Some((other, _)) = held.iter().find(|(i, k)| *k == key && *i != rec.issue) {
            return Ok(Some(format!(
                "names {key}, which ticket {other} already holds in the review loop"
            )));
        }
        // CAD-622: the apps this PR touches, and whether their teams name
        // the worker. Unreadable evidence holds the report — it never
        // admits an own-team author, and a known git source counts even
        // when the diff cannot be read.
        let evidence = self.resolve_apps(pm, &rec.issue, pr, sha, &[rec.worker.as_str()])?;
        match &evidence {
            crate::issue::app_guard::Evidence::Unavailable(why) => {
                rec.touched_apps = None;
                return Ok(Some(format!(
                    "could not be checked against its app's team: {why} — the report is \
                     held until the PR's own changed files can be read; re-file it once \
                     GitHub answers"
                )));
            }
            crate::issue::app_guard::Evidence::NotApplicable => {
                rec.touched_apps = None;
            }
            crate::issue::app_guard::Evidence::Known { apps, from_git } => {
                rec.touched_apps = Some(apps.iter().cloned().collect());
                for app in apps {
                    let touched = if from_git.contains(app) {
                        "its pinned git source".to_string()
                    } else {
                        format!("{}/{app}/", crate::issue::app::DIR)
                    };
                    if let Some(why) = crate::issue::app_guard::refusal(
                        crate::issue::app_guard::Ctx {
                            pm_dir: &pm.dir,
                            state_dir: Some(&self.state_dir),
                            store: Some(&self.store),
                        },
                        app,
                        &touched,
                        &rec.worker,
                    )? {
                        return Ok(Some(why));
                    }
                }
            }
        }
        Ok(None)
    }

    /// CAD-622: what this PR's head touches — its changed files under
    /// `apps/<name>/`, plus every app whose install record pins this
    /// PR's repository as its git source. `NotApplicable` when nobody in
    /// `aliases` is on any app's team and no app takes its source from
    /// this repository (nothing was read); `Unavailable` when the app
    /// question was asked and the diff could not answer — the caller
    /// holds, never admits.
    fn resolve_apps(
        &self,
        pm: &Pm,
        issue: &str,
        pr: &str,
        sha: &str,
        aliases: &[&str],
    ) -> Result<crate::issue::app_guard::Evidence> {
        use crate::issue::app_guard::{apps_from_git_source, apps_in_paths, teams, Evidence};
        let guard = crate::issue::app_guard::Ctx {
            pm_dir: &pm.dir,
            state_dir: Some(&self.state_dir),
            store: Some(&self.store),
        };
        let (slug, _) = crate::issue::task_report::parse_pr_url(pr)?;
        let from_git = apps_from_git_source(guard, &slug)?;
        let interested = !from_git.is_empty()
            || aliases.iter().any(|alias| {
                teams(guard).is_ok_and(|t| t.values().any(|team| team.contains(*alias)))
            });
        if !interested {
            return Ok(Evidence::NotApplicable);
        }
        let unavailable = |why: &str| {
            let _ = self.store.event_public(
                DAEMON_ALIAS,
                "review_app_guard_unavailable",
                json!({"issue": issue, "pr": pr, "head": sha, "why": why}),
            );
            Evidence::Unavailable(why.to_string())
        };
        let Ok(text) = delivery::gh(
            &self.delivery_gh.to_string_lossy(),
            &["pr", "view", pr, "--json", "files,headRefOid"],
        ) else {
            return Ok(unavailable("the PR's changed files could not be read"));
        };
        let view: Value = serde_json::from_str(&text)
            .map_err(|e| Error::rejected(format!("gh pr view: unreadable ({e})")))?;
        if view["headRefOid"].as_str() != Some(sha) {
            return Ok(unavailable("the PR tip moved while its files were read"));
        }
        let Some(files) = view["files"].as_array() else {
            return Ok(unavailable("the PR read returned no file list"));
        };
        let mut apps = from_git.clone();
        apps.extend(apps_in_paths(
            files.iter().filter_map(|f| f["path"].as_str()),
        ));
        Ok(Evidence::Known { apps, from_git })
    }

    /// The worker reported `sha` done on `pr`.
    fn on_done(
        self: &Arc<Self>,
        pm: &Pm,
        rec: &mut Record,
        sha: &str,
        pr: &str,
        outcome_report: &str,
        hist: &[&Record],
    ) -> Result<()> {
        let same_head = rec.head.as_deref() == Some(sha) && rec.pr.as_deref() == Some(pr);
        if same_head
            && matches!(
                rec.state,
                State::Reviewing | State::Passed | State::Ready | State::Enqueued
            )
        {
            return Ok(());
        }
        // A new head after review: whatever auto-merge is on was set for
        // the old head.
        if matches!(rec.state, State::Passed | State::Ready | State::Enqueued)
            && (rec.state == State::Enqueued || rec.observed.as_ref().is_some_and(|o| o.auto_merge))
        {
            rec.disable_auto = true;
        }
        rec.pr = Some(pr.to_string());
        rec.ready_evidence = None;
        rec.ready_binding = None;
        rec.outcome_report = (!outcome_report.is_empty()).then(|| outcome_report.to_string());
        if rec.head.as_deref() != Some(sha) {
            // A new head is a new diff — the risk tier is re-measured.
            rec.risk = None;
        }
        rec.head = Some(sha.to_string());
        rec.head_at = now();
        self.start_review(pm, rec, hist)
    }

    /// CAD-622: the candidates that are on the team of an app this
    /// head's PR touches — they never review a change to that app. The
    /// app set is resolved from the PR's own changed files (and known
    /// git sources) and persisted on the record, so the verdict rechecks
    /// the same head-bound evidence. `None` is unavailable evidence: the
    /// caller holds the review rather than routing it unchecked.
    fn app_barred(
        &self,
        pm: &Pm,
        rec: &mut Record,
        candidates: &[Candidate],
    ) -> Result<Option<std::collections::BTreeSet<String>>> {
        use crate::issue::app_guard::{teams, Evidence};
        let guard = crate::issue::app_guard::Ctx {
            pm_dir: &pm.dir,
            state_dir: Some(&self.state_dir),
            store: Some(&self.store),
        };
        let teams = teams(guard)?;
        let on_team: Vec<&String> = candidates
            .iter()
            .map(|c| &c.alias)
            .filter(|alias| teams.values().any(|team| team.contains(*alias)))
            .collect();
        if on_team.is_empty() && rec.touched_apps.is_none() {
            return Ok(Some(Default::default()));
        }
        let evidence = match rec.touched_apps.as_ref() {
            Some(apps) => Evidence::Known {
                apps: apps.iter().cloned().collect(),
                from_git: Default::default(),
            },
            None => {
                let (Some(pr), Some(head)) = (rec.pr.as_deref(), rec.head.as_deref()) else {
                    return Ok(Some(Default::default()));
                };
                let mut aliases: Vec<&str> = on_team.iter().map(|a| a.as_str()).collect();
                aliases.push(rec.worker.as_str());
                self.resolve_apps(pm, &rec.issue, pr, head, &aliases)?
            }
        };
        let apps = match evidence {
            Evidence::Unavailable(_) => return Ok(None),
            Evidence::NotApplicable => Default::default(),
            Evidence::Known { apps, .. } => apps,
        };
        rec.touched_apps = Some(apps.iter().cloned().collect());
        let mut barred = std::collections::BTreeSet::new();
        for app in &apps {
            let Some(team) = teams.get(app) else { continue };
            for alias in &on_team {
                if team.contains(*alias) {
                    barred.insert((*alias).clone());
                }
            }
        }
        Ok(Some(barred))
    }

    /// The head moved without a done report from the worker: whoever
    /// reviewed it may have pushed it, so that reviewer is barred from
    /// this ticket and the review goes to someone else.
    fn review_moved_head(
        self: &Arc<Self>,
        pm: &Pm,
        rec: &mut Record,
        hist: &[&Record],
    ) -> Result<()> {
        if let Some(prev) = rec.reviewer.take() {
            if !rec.excluded.contains(&prev) {
                rec.excluded.push(prev);
            }
        }
        // The head that was measured is gone — re-measure the new one.
        rec.risk = None;
        rec.ready_evidence = None;
        rec.ready_binding = None;
        rec.outcome_report = None;
        // CAD-622: the old head's changed files say nothing about this one.
        rec.touched_apps = None;
        self.start_review(pm, rec, hist)
    }

    /// `(files, additions, deletions)` for the head under review —
    /// the record's own observation when it names this head, else one
    /// bounded `gh pr view`. `None` means the diff could not be sized
    /// (offline, the head moved mid-read): the review proceeds without
    /// a tier rather than blocking on a stat fetch.
    fn review_stats(&self, rec: &Record) -> Option<(u64, u64, u64)> {
        let head = rec.head.as_deref()?;
        if let Some(o) = &rec.observed {
            if o.head == head {
                return Some((o.files, o.additions, o.deletions));
            }
        }
        let pr = rec.pr.as_deref()?;
        let text = delivery::gh(
            &self.delivery_gh.to_string_lossy(),
            &[
                "pr",
                "view",
                pr,
                "--json",
                "additions,deletions,changedFiles,headRefOid",
            ],
        )
        .ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        // The PR tip moved since `head` was recorded — the size read
        // belongs to a different diff; the observe path re-routes.
        if v["headRefOid"].as_str()? != head {
            return None;
        }
        Some((
            v["changedFiles"].as_u64()?,
            v["additions"].as_u64()?,
            v["deletions"].as_u64()?,
        ))
    }

    /// Route a review of `rec.head` to an independent reviewer, or mark
    /// the record unstaffed when nobody qualifies. CAD-362: an
    /// oversized diff is flagged for splitting instead — no reviewer
    /// reads a diff too big to verify.
    fn start_review(self: &Arc<Self>, pm: &Pm, rec: &mut Record, hist: &[&Record]) -> Result<()> {
        let (Some(sha), Some(pr)) = (rec.head.clone(), rec.pr.clone()) else {
            return Ok(());
        };
        rec.unattended_turn = None;
        let unattended = match self.unattended_authorization_for_issue(&rec.issue) {
            Ok(run) => run.is_some(),
            Err(error) => {
                rec.enter(State::Escalated, now());
                rec.unattended_hold = true;
                rec.note = Some(format!("Automatic review held: {error}"));
                return Ok(());
            }
        };
        if rec.unattended_hold {
            rec.unattended_hold = false;
            rec.note = None;
        }
        if unattended {
            match self.unattended_worker_complete(
                &rec.issue,
                &rec.worker,
                &mut rec.unattended_turn,
                &sha,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    rec.enter(State::Working, now());
                    return Ok(());
                }
                Err(error) => {
                    rec.enter(State::Escalated, now());
                    rec.unattended_hold = true;
                    rec.note = Some(format!("Correlated completion held: {error}"));
                    return Ok(());
                }
            }
        }
        // Ordinary routine work retains its lean readiness path. A workflow
        // grant requires independent QA even when the diff is routine.
        if let Some(report) = rec.outcome_report.as_deref().filter(|_| !unattended) {
            match delivery_requirements_rpc::routine_readiness(self, &rec.issue, &pr, &sha, report)
                .unwrap_or(RoutineReadiness::NotRoutine)
            {
                RoutineReadiness::Ready(evidence) => {
                    let binding = delivery::ReadyBinding {
                        repo: evidence.repo.clone(),
                        pr: evidence.pr,
                        head: evidence.head.clone(),
                        base: evidence.base.clone(),
                        base_ref: evidence.base_ref.clone(),
                        policy_digest: evidence.policy_digest.clone(),
                        ci_run_id: evidence.ci_run_id,
                        ci_run_url: evidence.ci_run_url.clone(),
                        outcome_report: evidence.outcome_report.clone(),
                    };
                    rec.ready_evidence = Some(evidence);
                    rec.ready_binding = Some(binding);
                    rec.reviewer = None;
                    rec.enter(State::Ready, now());
                    return Ok(());
                }
                RoutineReadiness::Waiting => {
                    rec.ready_evidence = None;
                    rec.ready_binding = None;
                    rec.reviewer = None;
                    rec.enter(State::Working, now());
                    return Ok(());
                }
                RoutineReadiness::NotRoutine => {
                    rec.ready_evidence = None;
                    rec.ready_binding = None;
                }
            }
        }
        // Risk is measured once per head — `on_done`/`review_moved_head`
        // clear it when the head moves.
        if rec.risk.is_none() {
            rec.risk = self
                .review_stats(rec)
                .map(|(files, adds, dels)| delivery::risk_class(adds, dels, files).as_str())
                .map(str::to_string);
        }
        if rec.risk.as_deref() == Some(delivery::Risk::Oversized.as_str()) {
            let why = "the diff is oversized for a single review — split it into \
                       reviewable pieces (smaller PRs), then file a new done report";
            rec.note = Some(format!("{sha} held from review: {why}"));
            // The flag is raised once — an Unstaffed record retries the
            // route every pass and must not re-comment each time.
            if rec.state != State::Unstaffed {
                rec.enter(State::Unstaffed, now());
                let _ = issue::write::add_comment(
                    pm,
                    &rec.issue,
                    &format!("Held from review at {sha}: {why}."),
                    Some(DAEMON_ALIAS),
                    Some("review"),
                    None,
                    DAEMON_ALIAS,
                );
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "review_oversized",
                    json!({"issue": rec.issue, "sha": sha, "pr": pr}),
                );
            }
            return Ok(());
        }
        // Profile-backed classification comes from the daemon's trusted
        // GitHub/base evidence and current approved policy, never record risk
        // or report metadata. Unavailable classification retains the strict
        // existing review route.
        let classification = delivery_requirements_rpc::classify(
            self,
            &json!({"issue": rec.issue, "pr": pr, "head": sha}),
        )
        .ok();
        let requirement = classification.as_ref().map(|v| &v["requirements"]);
        let profile_review = requirement
            .is_some_and(|r| r["activation"] == "active" && r["class"] == "consequential");
        let agents = self.store.agents()?;
        let worker_vendor = agents
            .iter()
            .find(|a| a.alias == rec.worker)
            .map(|a| delivery::vendor(&a.provider, a.model.as_deref()));
        let candidates: Vec<Candidate> = agents
            .iter()
            .map(|a| Candidate {
                alias: a.alias.clone(),
                provider: a.provider.clone(),
                model: a.model.clone(),
                state: a.state.clone(),
                enabled: a.enabled,
                upstream: super::agent_upstream(a).map(str::to_string),
                role: a.role.clone(),
            })
            .collect();
        // CAD-622: an app's own team agent never reviews a change to that
        // app. The PR's own changed files name the app; only candidates
        // that are on some app's team pay for that read.
        let Some(barred) = self.app_barred(pm, rec, &candidates)? else {
            // Unavailable evidence never routes an unchecked review: the
            // record is held (Unstaffed) and the next pass retries the
            // same read.
            if rec.state != State::Unstaffed {
                rec.enter(State::Unstaffed, now());
                rec.note = Some(format!(
                    "{sha} held from review: the PR's changed files could not be read, so \
                     the app-team check cannot be made"
                ));
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "review_app_guard_held",
                    json!({"issue": rec.issue, "sha": sha, "pr": pr}),
                );
            }
            return Ok(());
        };
        let mut excluded = rec.excluded.clone();
        excluded.extend(barred.iter().cloned());
        let Some(pick) = delivery::pick_reviewer(
            &rec.worker,
            worker_vendor.as_deref(),
            rec.reviewer.as_deref(),
            &excluded,
            &candidates,
            &pm.config.review,
            hist,
        ) else {
            if rec.state != State::Unstaffed {
                rec.enter(State::Unstaffed, now());
                let mut note = format!(
                    "no idle reviewer can take {sha}: a fresh review goes only to an idle \
                     agent registered with --role reviewer, never to a busy reviewer, an \
                     implementer, the author, or another PM's group"
                );
                if !barred.is_empty() {
                    note.push_str(&format!(
                        "; excluded as members of the app's own team: {}",
                        barred.iter().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
                rec.note = Some(note);
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "review_unstaffed",
                    json!({"issue": rec.issue, "sha": sha}),
                );
            }
            return Ok(());
        };
        let reviewer = pick.alias.clone();
        let agent = agents
            .iter()
            .find(|a| a.alias == reviewer)
            .ok_or_else(|| Error::internal("reviewer vanished"))?;
        let ticket = issue::board::find_issue(&pm.dir, &rec.issue)?;
        let items = issue::parse::acceptance_items(&ticket.body);
        let listing = issue::dispatch::acceptance_listing(&items);
        let round = rec.rounds + 1;
        let risk_note = if unattended {
            "\n\nUnattended grant: include `risk: auto` in the verdict frontmatter only if this exact head needs no sensitive/operator-only treatment. The path list is a floor: declare human/unknown risk for auth, identity, secrets or other consequential uncertainty; never label those auto."
        } else {
            ""
        };
        let mut text = delivery::review_kickoff(
            &rec.issue,
            round,
            &pr,
            &sha,
            &rec.worker,
            rec.risk.as_deref(),
            listing.as_deref(),
            &ticket.dir.join("issue.md"),
            store::kickoff_ceiling(&agent.provider, &agent.endpoint_kind)
                .saturating_sub(risk_note.len()),
        );
        if profile_review {
            text.push_str("\n\nRequired coverage: one combined Standards + Spec/security review of this exact PR head. Address both axes explicitly in the review.");
        }
        text.push_str(risk_note);
        let mid = format!("review-{}", hash(&format!("{}/{sha}/{round}", rec.issue)));
        self.send_as(
            &json!({"alias": reviewer, "text": text, "message": mid, "source": "review"}),
            &|_| Ok(store::Sender::Unattributed),
        )?;
        rec.rounds = round;
        rec.reviewer = Some(reviewer.clone());
        rec.note = if pick.reason.is_fallback() {
            Some(format!("reviewer {reviewer}: {}", pick.reason.describe()))
        } else {
            None
        };
        rec.enter(State::Reviewing, now());
        let mut cmt = format!("Review round {round} routed to {reviewer}: {pr} at {sha}");
        cmt.push_str(&format!(" [{}]", pick.reason.describe()));
        if let Some(risk) = rec.risk.as_deref() {
            cmt.push_str(&format!(" Risk: {risk}."));
        } else {
            cmt.push('.');
        }
        let _ = issue::write::add_comment(
            pm,
            &rec.issue,
            &cmt,
            Some(DAEMON_ALIAS),
            Some("review"),
            None,
            DAEMON_ALIAS,
        );
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "review_routed",
            json!({"issue": rec.issue, "reviewer": reviewer, "sha": sha, "pr": pr,
                   "round": round, "message": mid,
                   "reason": pick.reason.as_str(), "risk": rec.risk}),
        );
        Ok(())
    }

    /// Queue the recorded revision with its stable correlation id. A router
    /// retry after renewal/restart can never create a second revision turn.
    fn queue_revision(self: &Arc<Self>, rec: &mut Record, at: i64) -> Result<()> {
        let verdict = rec
            .verdict
            .as_ref()
            .ok_or_else(|| Error::rejected("a revision needs its recorded independent verdict"))?;
        let text = delivery::revise_message(
            &rec.issue,
            &verdict.reviewer,
            &verdict.sha,
            rec.revisions,
            &verdict.report,
            &verdict.summary,
            rec.pr.as_deref().unwrap_or_default(),
        );
        self.send_issue_followup(rec, &format!("revise-{}", hash(&verdict.report)), &text)?;
        rec.enter(State::Working, at);
        Ok(())
    }

    /// `report_verdict` — the reviewer's PASS/REVISE on the head under
    /// review. The caller is derived from the connection (never a field
    /// or `CADENCE_ALIAS`) and must be the assigned reviewer: not the
    /// worker, not the master, not the operator. The verdict's sha must
    /// be the head under review. Every refusal writes nothing.
    pub(super) fn rpc_report_verdict(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        super::reject_identity_fields(params, "verdict")?;
        let id = required_str(params, "issue")?;
        let text = required_str(params, "text")?;
        let who = match self.agent_caller(peer_pid, "verdict")? {
            AgentCaller::Agent(alias) => alias,
            AgentCaller::Operator => {
                return Err(Error::rejected(format!(
                    "a verdict comes from {id}'s assigned reviewer — the operator decides at \
                     the merge (`cadence delivery merge|decline {id}`), not by verdict"
                )));
            }
        };
        if master::is_master(&who) {
            return Err(Error::rejected(
                "the master never reviews — a verdict comes from the assigned reviewer",
            ));
        }
        let pm = self.pm()?;
        let prepared = task_report::prepare_verdict(&pm, text, id, &who)?;
        let sha = prepared.front.sha.clone().unwrap_or_default();
        let verdict = prepared
            .front
            .verdict
            .ok_or_else(|| Error::rejected("a verdict names pass or revise"))?;
        let _grant = self
            .unattended_lock
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let _g = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = delivery::load(&self.state_dir)?;
        let rec = all.get_mut(id).ok_or_else(|| not_in_loop(id))?;
        if rec.worker == who {
            return Err(Error::rejected(format!(
                "{who} is {id}'s worker — a worker never judges its own work"
            )));
        }
        if rec.state != State::Reviewing {
            return Err(Error::rejected(format!(
                "{id} is not under review (it is {}) — no verdict is due",
                rec.state.as_str()
            )));
        }
        if rec.reviewer.as_deref() != Some(who.as_str()) {
            return Err(Error::rejected(format!(
                "{id}'s review is assigned to {}, not {who}",
                rec.reviewer.as_deref().unwrap_or("nobody")
            )));
        }
        // CAD-622: an app's own team agent never reviews a change to that
        // app. The head's app set is resolved now when routing did not
        // persist one, membership is read fresh (an alias can join a team
        // while a review is outstanding), and unavailable evidence holds
        // the verdict rather than admitting it.
        {
            use crate::issue::app_guard::{review_refusal, Evidence};
            let guard = crate::issue::app_guard::Ctx {
                pm_dir: &pm.dir,
                state_dir: Some(&self.state_dir),
                store: Some(&self.store),
            };
            let evidence = match rec.touched_apps.as_ref() {
                Some(apps) => Evidence::Known {
                    apps: apps.iter().cloned().collect(),
                    from_git: Default::default(),
                },
                None => match (rec.pr.as_deref(), rec.head.as_deref()) {
                    (Some(pr), Some(head)) => {
                        self.resolve_apps(&pm, id, pr, head, &[who.as_str(), rec.worker.as_str()])?
                    }
                    _ => Evidence::NotApplicable,
                },
            };
            match evidence {
                Evidence::Unavailable(why) => {
                    return Err(Error::rejected(format!(
                        "{id}'s verdict is held: the PR's changed files could not be read \
                         ({why}) — the app-team check is never skipped; file it again when \
                         GitHub answers"
                    )));
                }
                Evidence::Known { apps, .. } => {
                    for app in apps {
                        if let Some(why) = review_refusal(guard, &app, &who)? {
                            return Err(Error::rejected(why));
                        }
                    }
                }
                Evidence::NotApplicable => {}
            }
        }
        let head = rec.head.clone().unwrap_or_default();
        if sha != head {
            return Err(Error::rejected(format!(
                "stale verdict: it judges {sha}, but the head under review is {head} — \
                 review {head} and file the verdict for it"
            )));
        }
        let authorization = self.unattended_authorization_for_issue(id);
        if matches!(&authorization, Ok(Some(_)))
            && verdict == task_report::Verdict::Pass
            && prepared.front.risk.is_none()
        {
            return Err(Error::rejected(
                "an unattended PASS needs an explicit verdict-frontmatter risk declaration",
            ));
        }
        let filed = task_report::store(&pm, &prepared, &who)?;
        let at = now();
        let summary = task_report::summary_line(prepared.body(), delivery::SUMMARY_MAX);
        let report = filed["path"].as_str().unwrap_or_default().to_string();
        rec.verdict = Some(VerdictRec {
            verdict: verdict.as_str().to_string(),
            sha: sha.clone(),
            reviewer: who.clone(),
            summary: summary.clone(),
            report: report.clone(),
            at,
        });
        let comment = match (authorization, verdict) {
            (Err(error), _) => {
                if matches!(verdict, task_report::Verdict::Revise) {
                    rec.revisions += 1;
                }
                rec.enter(State::Escalated, at);
                rec.unattended_hold = true;
                rec.note = Some(format!("Automatic continuation held: {error}"));
                format!(
                    "{} by {who} at {sha}: {summary} — held for workflow authorization",
                    verdict.as_str()
                )
            }
            (Ok(_), task_report::Verdict::Pass) => {
                rec.enter(State::Passed, at);
                format!("PASS by {who} at {sha}: {summary}")
            }
            (Ok(_), task_report::Verdict::Revise) => {
                rec.revisions += 1;
                if rec.revisions >= delivery::MAX_REVISE {
                    rec.enter(State::Escalated, at);
                    rec.note = Some(format!(
                        "{} REVISE verdicts — the review did not converge; the operator decides",
                        rec.revisions
                    ));
                    format!(
                        "REVISE {} of {} by {who} at {sha}: {summary} — escalated to the \
                         operator",
                        rec.revisions,
                        delivery::MAX_REVISE
                    )
                } else {
                    self.queue_revision(rec, at)?;
                    format!(
                        "REVISE {} of {} by {who} at {sha}: {summary} — back to {}",
                        rec.revisions,
                        delivery::MAX_REVISE,
                        rec.worker
                    )
                }
            }
        };
        let review_receipt = json!({
            "issue": id, "verdict": verdict.as_str(), "sha": sha, "reviewer": who,
            "report": report, "pr": rec.pr, "project": rec.project, "worker": rec.worker,
            "risk": prepared.front.risk,
        });
        let out = rec.to_json();
        delivery::save(&self.state_dir, &all)?;
        // CAD-449: the daemon's own statement of this verdict — what a
        // merge must match to mark the ticket done.
        if let Err(e) = self.store.record_review_verdict(review_receipt) {
            tracing::warn!("verdict evidence for {id}: {e}");
        }
        let _ = issue::write::add_comment(
            &pm,
            id,
            &comment,
            Some(DAEMON_ALIAS),
            Some("review"),
            None,
            DAEMON_ALIAS,
        );
        // The master hears of the verdict from here, never from a file
        // under reports/.
        if self.store.agent_opt(master::ALIAS).ok().flatten().is_some() {
            let text = format!(
                "[report] {id} verdict {} by {who} at {sha}\nReport: {report}\n\n{comment}",
                verdict.as_str()
            );
            let _ = self.send_as(
                &json!({"alias": master::ALIAS, "text": text,
                        "message": format!("verdict-{}", hash(&report)), "source": "report"}),
                &|_| Ok(store::Sender::Unattributed),
            );
        }
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "review_verdict",
            json!({"issue": id, "verdict": verdict.as_str(), "sha": sha, "reviewer": who,
                   "report": report}),
        );
        self.wake();
        let mut filed = filed;
        filed["delivery"] = out;
        Ok(filed)
    }

    /// `delivery_list` — every record, oldest dispatch first. A read.
    /// CAD-437: `issues`/`states`/`projects` are repeatable any-of
    /// filters; `open` keeps the loop's live rows (non-terminal).
    pub(super) fn rpc_delivery_list(&self, params: &Value) -> Result<Value> {
        let mut issues = optional_strs(params, "issues")?;
        if let Some(one) = optional_str(params, "issue") {
            issues.push(one.to_string());
        }
        let states = optional_strs(params, "states")?;
        check_values("states", &states, &State::all_str())?;
        let projects = optional_strs(params, "projects")?;
        let open = params.get("open").and_then(Value::as_bool) == Some(true);
        let mut rows: Vec<Record> = delivery::load(&self.state_dir)?
            .into_values()
            .filter(|r| issues.is_empty() || issues.contains(&r.issue))
            .collect();
        if !projects.is_empty() {
            // The valid set: the tracker's project keys plus whatever
            // projects live records carry — a project deleted from the
            // tracker still names its rows, and a name in neither is
            // an unknown value (the grammar's error), not an empty page.
            let mut valid: std::collections::BTreeSet<String> =
                rows.iter().map(|r| r.project.clone()).collect();
            if let Ok(dir) = self.pm_dir() {
                if let Ok(list) = issue::project::list(&dir) {
                    valid.extend(list.into_iter().map(|p| p.key));
                }
            }
            let refs: Vec<&str> = valid.iter().map(String::as_str).collect();
            check_values("projects", &projects, &refs)?;
        }
        rows.retain(|r| projects.is_empty() || projects.contains(&r.project));
        rows.retain(|r| states.is_empty() || states.iter().any(|s| s == r.state.as_str()));
        rows.retain(|r| !open || !r.state.terminal());
        rows.sort_by_key(|r| r.dispatched_at);
        // CAD-446: the tracker whose project remotes this daemon checks
        // PRs against — the board's unattended sync checks the same one.
        Ok(json!({
            "records": rows.iter().map(Record::to_json).collect::<Vec<_>>(),
            "pm_dir": self.pm_dir().ok(),
        }))
    }

    /// `delivery_observe` — the operator's process reports what GitHub
    /// shows for a ticket's PR. A head that moved after review re-enters
    /// review; the answer's `disable_auto` asks the process to turn off
    /// auto-merge set for a head nobody approved.
    pub(super) fn rpc_delivery_observe(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("delivery observe", params, peer_pid)?;
        let id = required_str(params, "issue")?;
        let head = store::check_commit_sha(required_str(params, "head")?)?;
        let pr_state = required_str(params, "pr_state")?;
        if !matches!(pr_state, "OPEN" | "MERGED" | "CLOSED") {
            return Err(Error::rejected(format!(
                "pr_state is OPEN, MERGED or CLOSED, not '{pr_state}'"
            )));
        }
        let flag = |f: &str| params.get(f).and_then(Value::as_bool).unwrap_or(false);
        let count = |f: &str| params.get(f).and_then(Value::as_u64).unwrap_or(0);
        let obs = Observed {
            head: head.clone(),
            pr_state: pr_state.to_string(),
            ci_green: flag("ci_green"),
            auto_merge: flag("auto_merge"),
            additions: count("additions"),
            deletions: count("deletions"),
            files: count("files"),
            at: now(),
            // CAD-564: absent means "the caller did not stamp it" —
            // treat the read as fresh (now), so an older cadence's
            // sync still sees post-review head moves; only a read
            // demonstrably begun before `head_at` is stale.
            read_at: params["read_at"].as_i64().unwrap_or_else(now),
        };
        let pm = self.pm()?;
        let grant_guard = self
            .unattended_lock
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let guard = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = delivery::load(&self.state_dir)?;
        // CAD-362: pair-precision history for any re-route below.
        let history: Vec<Record> = all.values().cloned().collect();
        let hist: Vec<&Record> = history.iter().collect();
        let rec = all.get_mut(id).ok_or_else(|| not_in_loop(id))?;
        let before = rec.state;
        // CAD-776: readiness before this observation — the wake fires
        // on the transition into merge-ready, never on merely being
        // there.
        let was_ready = rec.merge_ready();
        let mut moved = rec.head.as_deref() != Some(head.as_str());
        // CAD-564: an observation whose read began before the record's
        // last head change (the done report's `head_at`) saw the head
        // that change recorded — applied after it, it is not a
        // post-review move and never rewinds the review.
        if moved && obs.read_at < rec.head_at {
            moved = false;
        }
        if !rec.state.terminal() {
            match pr_state {
                "MERGED" => rec.enter(State::Merged, obs.at),
                "CLOSED" => rec.enter(State::Closed, obs.at),
                _ if moved
                    && matches!(
                        rec.state,
                        State::Reviewing | State::Passed | State::Ready | State::Enqueued
                    ) =>
                {
                    rec.head = Some(head.clone());
                    rec.head_at = now();
                    self.review_moved_head(&pm, rec, &hist)?;
                    let _ = issue::write::add_comment(
                        &pm,
                        id,
                        &format!(
                            "The PR head moved to {head} after review ({}) — back to review.",
                            before.as_str()
                        ),
                        Some(DAEMON_ALIAS),
                        Some("review"),
                        None,
                        DAEMON_ALIAS,
                    );
                }
                _ => {}
            }
        }
        let was_disable = rec.disable_auto;
        rec.observed = Some(obs);
        if matches!(rec.state, State::Ready | State::Enqueued)
            && (rec.ready_evidence.is_some() || rec.state == State::Ready)
        {
            let readiness = match (rec.pr.as_deref(), rec.outcome_report.as_deref()) {
                (Some(pr), Some(report)) => {
                    delivery_requirements_rpc::routine_readiness(self, id, pr, &head, report)
                        .unwrap_or(RoutineReadiness::NotRoutine)
                }
                _ => RoutineReadiness::NotRoutine,
            };
            match readiness {
                RoutineReadiness::Ready(evidence) => {
                    apply_routine_ready_observation(rec, evidence, was_disable, now());
                }
                RoutineReadiness::Waiting => {
                    rec.ready_binding = None;
                    rec.ready_evidence = None;
                    rec.enter(State::Working, now());
                }
                RoutineReadiness::NotRoutine => {
                    rec.ready_binding = None;
                    rec.ready_evidence = None;
                    rec.enter(State::Working, now());
                    self.start_review(&pm, rec, &hist)?;
                }
            }
        }
        let (unattended_authorized, unattended_refusal) =
            match self.try_unattended_enqueue(&pm, rec) {
                Ok(authorized) => (authorized, None),
                Err(error) => {
                    let reason = error.to_string();
                    if !rec.unattended_hold || rec.note.as_deref() != Some(reason.as_str()) {
                        self.store.event_public(
                            store::APPROVAL_STREAM,
                            "unattended_landing_held",
                            json!({"issue": id, "head": rec.head, "reason": reason}),
                        )?;
                    }
                    rec.unattended_hold = true;
                    rec.note = Some(reason.clone());
                    (false, Some(reason))
                }
            };
        // Preserve an authorized pending request without calling it Enqueued.
        let routine_approved = rec.state == State::Enqueued
            && rec
                .ready_evidence
                .as_ref()
                .zip(rec.ready_binding.as_ref())
                .is_some_and(|(e, b)| {
                    e.repo == b.repo
                        && e.pr == b.pr
                        && e.head == b.head
                        && e.base == b.base
                        && e.policy_digest == b.policy_digest
                        && e.ci_run_id == b.ci_run_id
                        && e.ci_run_url == b.ci_run_url
                        && e.outcome_report == b.outcome_report
                        && e.head == head
                });
        let approved = (rec.state == State::Enqueued
            && (rec.passed_sha() == Some(head.as_str()) || routine_approved))
            || (unattended_authorized && rec.passed_sha() == Some(head.as_str()));
        rec.disable_auto = rec.observed.as_ref().is_some_and(|o| o.auto_merge)
            && !approved
            && rec.state != State::Merged;
        // CAD-776: count the transition into merge-ready here, under
        // `delivery_lock`, so the epoch persists atomically with the
        // observation that caused it — every later wake attempt reads
        // the same saved key.
        rec.advance_ready_epoch(was_ready);
        let mut out = json!({
            "issue": id, "state": rec.state.as_str(), "was": before.as_str(),
            "disable_auto": rec.disable_auto, "merge_ready": rec.merge_ready(),
        });
        if let Some(reason) = unattended_refusal {
            out["unattended_refusal"] = json!(reason);
        }
        // CAD-449: only the transition into `merged` settles the ticket's
        // status. The record's `merged` is the once-guard: a replay, a
        // second sync or a ticket reopened after its merge finds it
        // merged already and writes nothing. The tracker write comes
        // first — if the save then fails, the next sync transitions
        // again and finds the ticket done (`kept`).
        let mut notices = Vec::new();
        // CAD-445 + CAD-449: `wake_lock` is held from before the done
        // write until the loop-end wake records the dependents it named,
        // so the router's blocker-done pass cannot wake them a second time.
        let merging = before != State::Merged && rec.state == State::Merged;
        let wake_guard = merging.then(|| self.wake_lock.lock().unwrap_or_else(|e| e.into_inner()));
        if merging {
            let done = self.settle_merge(&pm, rec, before, &head, &mut notices);
            out["ticket"] = serde_json::to_value(&done).unwrap_or(Value::Null);
            rec.ticket_done = Some(done);
        }
        // CAD-445: a loop that just ended wakes the master once — after
        // the ticket's status is settled, so the wake reads it done.
        let ended = (before != rec.state).then(|| rec.clone());
        delivery::save(&self.state_dir, &all)?;
        drop(guard);
        drop(grant_guard);
        // The wake first: it releases `wake_lock`, which a comment waiting
        // on the tracker lock must not hold.
        if let Some(rec) = ended {
            self.wake_on_delivery_end(&rec, wake_guard);
        } else {
            drop(wake_guard);
        }
        // CAD-776: the observation made the delivery merge-ready — one
        // durable hint to the master per readiness streak. A loop that
        // just ended is terminal, never merge-ready, so the two wakes
        // cannot fire together. The key comes from the saved record
        // alone, so concurrent observers converge on it.
        if let Some(ready) = all
            .get(id)
            .filter(|r| r.merge_ready() && r.state != State::Enqueued && !r.unattended_hold)
        {
            self.wake_on_merge_ready(ready);
        }
        self.post_notices(&pm, notices);
        if rec_state_changed(&out, was_disable) {
            let _ = self
                .store
                .event_public(DAEMON_ALIAS, "delivery_observed", out.clone());
            self.wake();
        }
        Ok(out)
    }

    /// CAD-449, the router pass: a merged loop whose ticket is not
    /// settled — `pending` (the tracker write failed: busy, a failing
    /// hook) gets two retries, with a retained known status; `refused`
    /// becomes `kept` once the
    /// operator set the status by hand. Answers how many records moved.
    pub(super) fn settle_ticket_done(&self) -> Result<usize> {
        let _grant = self
            .unattended_lock
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let (moved, notices, pm) = {
            let _g = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut all = delivery::load(&self.state_dir)?;
            let open: Vec<String> = all
                .values()
                .filter(|r| r.state == State::Merged)
                .filter(|r| r.ticket_done.as_ref().is_some_and(|d| d.open().is_some()))
                .map(|r| r.issue.clone())
                .collect();
            if open.is_empty() {
                return Ok(0);
            }
            let pm = self.pm()?;
            let mut notices = Vec::new();
            let mut moved = 0;
            #[cfg(feature = "test-seam")]
            let mut saved_second_retries = Vec::new();
            for id in open {
                let Some(rec) = all.get_mut(&id) else {
                    continue;
                };
                let next = match rec.ticket_done.clone() {
                    Some(TicketDone::Pending {
                        why,
                        from,
                        attempts,
                    }) => {
                        // Only while the status is still what it was at
                        // the merge: a status set by hand since (the
                        // `merged_not_done` row sends the operator here)
                        // is the operator's decision, never overwritten.
                        let Some(from) = from else {
                            notices.push(Notice {
                                issue: id.clone(),
                                comment: None,
                                kind: "ticket_done_refused",
                                payload: json!({"issue": id, "why": "status at merge unknown"}),
                            });
                            rec.ticket_done = Some(TicketDone::Refused {
                                why: format!(
                                    "the done write failed ({why}) and the status at the \
                                     merge is unknown — the operator sets it"
                                ),
                            });
                            moved += 1;
                            continue;
                        };
                        if attempts >= delivery::MAX_DONE_ATTEMPTS {
                            rec.ticket_done = Some(TicketDone::Refused {
                                why: format!(
                                    "the done write failed after {} attempts ({why}) — the operator sets it",
                                    delivery::MAX_DONE_ATTEMPTS
                                ),
                            });
                            moved += 1;
                            continue;
                        }
                        let head = rec.observed.as_ref().map(|o| o.head.clone());
                        let refusal = match self
                            .unattended_merge_done_refusal(rec, head.as_deref().unwrap_or_default())
                        {
                            Ok(refusal) => refusal,
                            Err(error) => Some(format!(
                                "the unattended merge could not be checked: {error}"
                            )),
                        };
                        if let Some(why) = refusal {
                            notices.push(Notice {
                                issue: id.clone(),
                                comment: None,
                                kind: "ticket_done_refused",
                                payload: json!({"issue": id, "why": why}),
                            });
                            rec.ticket_done = Some(TicketDone::Refused { why });
                            moved += 1;
                            continue;
                        }
                        let mut mine = Vec::new();
                        let mut next = self.mark_ticket_done(
                            &pm,
                            rec,
                            &head.unwrap_or_default(),
                            Some(&from),
                            &mut mine,
                        );
                        // Persist the failure count even when its reason
                        // is unchanged; do not repeat the same notice.
                        if let TicketDone::Pending {
                            attempts: next_attempts,
                            ..
                        } = &mut next
                        {
                            *next_attempts = attempts.max(1).saturating_add(1);
                            if matches!(&next, TicketDone::Pending { why: now, .. } if *now == why)
                            {
                                mine.clear();
                            }
                            if attempts.max(1).saturating_add(1) >= delivery::MAX_DONE_ATTEMPTS {
                                let failure = next.open().unwrap_or_default();
                                let reason = format!(
                                    "the done write failed after {} attempts ({failure}) — the operator sets it",
                                    delivery::MAX_DONE_ATTEMPTS
                                );
                                mine.push(Notice {
                                    issue: id.clone(),
                                    comment: Some(reason.clone()),
                                    kind: "ticket_done_refused",
                                    payload: json!({"issue": id, "why": reason}),
                                });
                                next = TicketDone::Refused { why: reason };
                            }
                        }
                        let comment = match &next {
                            TicketDone::Marked { .. } => Some(format!(
                                "{id} marked done after a retry (the first write failed: {why})."
                            )),
                            TicketDone::Kept { status }
                                if status != "done" && status != "dropped" =>
                            {
                                Some(format!(
                                    "{id} was not marked done: its status was set to {status} \
                                     by hand after the merge (it was {from}); left as it is."
                                ))
                            }
                            _ => None,
                        };
                        if let Some(text) = comment {
                            notices.push(Notice {
                                issue: id.clone(),
                                comment: Some(text),
                                kind: "ticket_done_retried",
                                payload: json!({"issue": id, "why": why, "outcome": next}),
                            });
                        }
                        notices.extend(mine);
                        next
                    }
                    Some(TicketDone::Refused { .. }) => {
                        match issue::board::find_issue(&pm.dir, &id) {
                            Ok(t) if matches!(t.front.status.as_str(), "done" | "dropped") => {
                                TicketDone::Kept {
                                    status: t.front.status.clone(),
                                }
                            }
                            _ => continue,
                        }
                    }
                    _ => continue,
                };
                rec.ticket_done = Some(next);
                #[cfg(feature = "test-seam")]
                if matches!(
                    rec.ticket_done.as_ref(),
                    Some(TicketDone::Pending { attempts: 2, .. })
                ) {
                    saved_second_retries.push(id.clone());
                }
                moved += 1;
            }
            if moved > 0 {
                delivery::save(&self.state_dir, &all)?;
            }
            #[cfg(feature = "test-seam")]
            if let Some(pause) = &self.after_done_retry_saved {
                for id in saved_second_retries {
                    pause(&id);
                }
            }
            (moved, notices, pm)
        };
        self.post_notices(&pm, notices);
        if moved > 0 {
            self.wake();
        }
        Ok(moved)
    }

    /// CAD-449: the operator's process just saw `rec`'s PR merged at
    /// `head` (the record left `before` for `merged`). The ticket becomes
    /// `done` when the merge is the reviewed one — see
    /// [`Self::merge_done_refusal`] — else it is `refused` and a ticket
    /// comment says why. Runs under `delivery_lock`; what to tell goes to
    /// `notices`, posted after the lock is released.
    fn settle_merge(
        &self,
        pm: &Pm,
        rec: &Record,
        before: State,
        head: &str,
        notices: &mut Vec<Notice>,
    ) -> TicketDone {
        let refused = match self.merge_done_refusal(pm, rec, before, head) {
            Ok(r) => r,
            Err(e) => Some(format!("the merge could not be checked: {e}")),
        };
        let Some(why) = refused else {
            return self.mark_ticket_done(pm, rec, head, None, notices);
        };
        let pr_ref = rec_pr_ref(rec);
        let id = &rec.issue;
        notices.push(Notice {
            issue: id.clone(),
            comment: Some(format!(
                "{pr_ref} merged at {head}, but {id} was not marked done: {why}. \
                 The operator sets its status."
            )),
            kind: "ticket_done_refused",
            payload: json!({"issue": id, "pr": pr_ref, "head": head, "why": why}),
        });
        TicketDone::Refused { why }
    }

    /// The tracker write for a reviewed merge: `marked`, `kept` (already
    /// done or dropped, or — with `expect` — no longer that status) or
    /// `pending` (the write failed; retried by the router pass while the
    /// status is still what it was). The commit's `Actor:` names the observer and the
    /// delivery. Never waits for the tracker lock.
    fn mark_ticket_done(
        &self,
        pm: &Pm,
        rec: &Record,
        head: &str,
        expect: Option<&str>,
        notices: &mut Vec<Notice>,
    ) -> TicketDone {
        let id = &rec.issue;
        let pr_ref = rec_pr_ref(rec);
        let actor = format!("operator (delivery {pr_ref})");
        let why = format!("{pr_ref} merged at {head}");
        // CAD-1609: the close writes ONE issue.md, so the lock is
        // scoped to it. A crashed writer's leftovers on OTHER paths no
        // longer refuse the close, while leftovers on this ticket still
        // do. The daemon never waits for the lock either way.
        let scoped = crate::issue::write::issue_dir(pm, id)
            .ok()
            .map(|(_, dir)| dir.join("issue.md"));
        let lock = match &scoped {
            Some(path) => pm.try_lock_for_paths(std::slice::from_ref(path)),
            // The ticket's folder is gone: the write below says so.
            None => pm.try_lock(),
        }
        .and_then(|lock| {
            lock.ok_or_else(|| {
                Error::rejected("the tracker is locked by another writer (write lock)")
            })
        });
        let result = match lock {
            Ok(tracker) => {
                // Remote collection has finished. Grant→delivery→tracker:
                // revalidate locally inside the actual status-write snapshot.
                let authority = self.unattended_authorization_for_issue(id).and_then(|run| {
                    if run.is_some_and(|run| now() as f64 >= run.expires_at) {
                        Err(Error::rejected("the bound grant expired before settlement"))
                    } else {
                        Ok(())
                    }
                });
                if let Err(error) = authority {
                    let why = format!(
                        "unattended postmerge verification held: {error}; operator verification is required"
                    );
                    notices.push(Notice {
                        issue: id.clone(),
                        comment: None,
                        kind: "ticket_done_refused",
                        payload: json!({"issue": id, "why": why}),
                    });
                    return TicketDone::Refused { why };
                }
                issue::write::mark_done_on_merge_locked(pm, &tracker, id, &why, &actor, expect)
            }
            Err(error) => Err(issue::write::DoneWriteFailure { error, from: None }),
        };
        match result {
            Ok(None) => {
                notices.push(Notice {
                    issue: id.clone(),
                    comment: None,
                    kind: "ticket_done_on_merge",
                    payload: json!({"issue": id, "pr": pr_ref, "head": head, "actor": actor}),
                });
                TicketDone::Marked { at: now() }
            }
            Ok(Some(status)) => TicketDone::Kept { status },
            Err(e) => {
                #[cfg(feature = "test-seam")]
                if let Some(pause) = &self.after_done_write_failure {
                    pause();
                }
                let why = e.to_string();
                notices.push(Notice {
                    issue: id.clone(),
                    comment: Some(format!(
                        "{pr_ref} merged at {head}, but marking {id} done failed: {why}. \
                         The daemon allows two retries; the operator can set its status."
                    )),
                    kind: "ticket_done_pending",
                    payload: json!({"issue": id, "pr": pr_ref, "head": head, "why": why}),
                });
                // Never re-read after releasing the transaction lock:
                // that would adopt an intervening writer's decision.
                // A busy retry may retain its already captured expect.
                let from = e.from.or_else(|| expect.map(str::to_string));
                if from.is_none() {
                    let reason = format!(
                        "the done write failed ({why}) before its status was read under lock — the operator sets it"
                    );
                    notices.pop();
                    notices.push(Notice {
                        issue: id.clone(),
                        comment: None,
                        kind: "ticket_done_refused",
                        payload: json!({"issue": id, "why": reason}),
                    });
                    TicketDone::Refused { why: reason }
                } else {
                    TicketDone::Pending {
                        why,
                        from,
                        attempts: 1,
                    }
                }
            }
        }
    }

    /// Post what [`Self::settle_merge`] had to tell, outside
    /// `delivery_lock`: a comment waits for the tracker lock like any
    /// writer, and must not hold the loop up while it does.
    fn post_notices(&self, pm: &Pm, notices: Vec<Notice>) {
        for n in notices {
            // A write that failed on a busy tracker would wait out the
            // same lock here; the Needs-you row and the event carry it,
            // and the retry that settles it comments then.
            let busy = matches!(n.kind, "ticket_done_pending" | "ticket_done_refused")
                && !matches!(pm.lock_state(), issue::LockState::Free);
            if let Some(text) = n.comment.as_ref().filter(|_| !busy) {
                let _ = issue::write::add_comment(
                    pm,
                    &n.issue,
                    text,
                    Some(DAEMON_ALIAS),
                    Some("review"),
                    None,
                    DAEMON_ALIAS,
                );
            }
            let _ = self.store.event_public(DAEMON_ALIAS, n.kind, n.payload);
        }
    }

    /// Why a merge observed for `rec` does not mark its ticket done, if
    /// it does not. Allowlist: the loop stood on a PASS (`passed` or
    /// `enqueued` before the merge — never a review in progress, an
    /// escalation or an unstaffed review); the merged head is the
    /// PASSed sha; the PR is in the project's own repos (the check a
    /// done report passes, run again now); and the PASS is one
    /// `report_verdict` recorded — the store's [`store::VERDICT_STREAM`]
    /// holds that verdict, sha, reviewer and report, written from the
    /// identity the daemon derived from the reviewer's connection. The
    /// record is never trusted on its own: `delivery.json` and the
    /// tracker are files, and neither carries the daemon's statement.
    fn merge_done_refusal(
        &self,
        pm: &Pm,
        rec: &Record,
        before: State,
        head: &str,
    ) -> Result<Option<String>> {
        if !matches!(before, State::Passed | State::Enqueued) {
            return Ok(Some(format!(
                "the loop was {} when it merged, not passed or enqueued",
                before.as_str()
            )));
        }
        let Some(v) = rec.verdict.as_ref().filter(|v| v.verdict == "pass") else {
            return Ok(Some("no PASS verdict stands".to_string()));
        };
        if v.sha != head {
            return Ok(Some(format!(
                "the merged head {head} is not the reviewed {}",
                v.sha
            )));
        }
        let Some(pr) = rec.pr.as_deref() else {
            return Ok(Some("the loop records no PR".to_string()));
        };
        let mut owned = rec.clone();
        if let Some(why) = self.pr_refusal(pm, &mut owned, pr, head, &[])? {
            return Ok(Some(format!("the PR {why}")));
        }
        if !self
            .store
            .verdict_recorded(&rec.issue, "pass", head, &v.reviewer, &v.report)?
        {
            return Ok(Some(format!(
                "the daemon recorded no PASS by {} for {head} ({})",
                v.reviewer, v.report
            )));
        }
        if let Some(why) = self.unattended_merge_done_refusal(rec, head)? {
            return Ok(Some(why));
        }
        Ok(None)
    }

    /// `delivery_merge` — the operator's merge decision, in phases run
    /// by `cadence delivery merge` from the operator's process:
    /// `authorize` (a PASS stands), `check` (and GitHub shows that head
    /// open and green — answers the PR and the sha to pin), `enqueued`
    /// (the operator's `gh` enqueued it pinned to `sha`).
    pub(super) fn rpc_delivery_merge(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("delivery merge", params, peer_pid)?;
        let id = required_str(params, "issue")?;
        let phase = required_str(params, "phase")?;
        let _g = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = delivery::load(&self.state_dir)?;
        let rec = all.get_mut(id).ok_or_else(|| not_in_loop(id))?;
        if !matches!(rec.state, State::Passed | State::Ready) {
            return Err(Error::rejected(format!(
                "{id} has no standing PASS or routine readiness to merge (it is {})",
                rec.state.as_str()
            )));
        }
        let sha = if rec.state == State::Ready {
            if !rec.merge_ready() {
                return Err(Error::rejected(format!(
                    "{id} routine readiness is stale; sync and retry"
                )));
            }
            rec.ready_evidence
                .as_ref()
                .map(|e| e.head.clone())
                .unwrap_or_default()
        } else {
            rec.passed_sha().unwrap_or_default().to_string()
        };
        let pr = rec.pr.clone().unwrap_or_default();
        if rec.state == State::Ready && phase != "authorize" {
            return Err(Error::rejected(format!(
                "{id} routine readiness must be revalidated by delivery_approve"
            )));
        }
        match phase {
            "authorize" => Ok(json!({"issue": id, "sha": sha, "pr": pr})),
            "check" | "enqueued" => {
                if !rec.merge_ready() {
                    let why = match &rec.observed {
                        None => "GitHub has not been read for it yet".to_string(),
                        Some(o) if o.head != sha => {
                            format!("the PR head is {}, not the reviewed {sha}", o.head)
                        }
                        Some(o) if o.pr_state != "OPEN" => format!("the PR is {}", o.pr_state),
                        Some(_) => "its CI is not green".to_string(),
                    };
                    return Err(Error::rejected(format!(
                        "{id} is not ready to merge: {why}"
                    )));
                }
                // CAD-1561: the retained check/enqueued phases share the
                // requirements classifier's authoritative current-target
                // admission — a standing PASS on a base that is neither
                // the repository default nor the approved
                // `integration_branch` refuses here exactly as
                // `delivery_approve` refuses it.
                delivery_requirements_rpc::classify(
                    self,
                    &json!({"issue": id, "pr": pr, "head": sha}),
                )
                .map_err(|_| {
                    Error::rejected(
                        "current profile requirements could not be verified; the merge was not enqueued",
                    )
                })?;
                if phase == "check" {
                    return Ok(json!({"issue": id, "sha": sha, "pr": pr}));
                }
                if optional_str(params, "sha") != Some(sha.as_str()) {
                    return Err(Error::rejected(format!(
                        "{id}'s reviewed head is {sha} — the enqueue must be pinned to it"
                    )));
                }
                rec.enter(State::Enqueued, now());
                rec.unattended_hold = false;
                let out = rec.to_json();
                delivery::save(&self.state_dir, &all)?;
                if let Ok(pm) = self.pm() {
                    let _ = issue::write::add_comment(
                        &pm,
                        id,
                        &format!("Merge enqueued by the operator: {pr}, pinned to {sha}."),
                        Some("operator"),
                        Some("review"),
                        None,
                        "operator",
                    );
                }
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "merge_enqueued",
                    json!({"issue": id, "sha": sha, "pr": pr}),
                );
                self.wake();
                Ok(out)
            }
            other => Err(Error::rejected(format!(
                "phase is authorize, check or enqueued, not '{other}'"
            ))),
        }
    }

    /// `delivery_approve` (CAD-140) — the operator's merge approval as
    /// one single-writer transaction under `delivery_lock`: re-read the
    /// PR, compare-and-swap the shown head against the live one,
    /// record the approval object, and enqueue pinned to it. `sha` is
    /// REQUIRED — the head the approval was made against; a moved head
    /// voids the decision (`head_moved`) before anything is recorded.
    /// The approval carries the deciding actor (`request_actor`,
    /// attribution only — authority is this connection). Neither the
    /// board nor the CLI shells `gh` for a merge anymore; the daemon
    /// shells it only here, bound to the recorded approval. A refused
    /// approval writes nothing: no record, no observation, no enqueue.
    pub(super) fn rpc_delivery_approve(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("delivery approve", params, peer_pid)?;
        let id = required_str(params, "issue")?;
        let want = params
            .get("sha")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::rejected("delivery approve names the approved head: {\"sha\"} is required")
            })?;
        if want.len() != 40
            || !want
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::rejected(
                "approved head must be the full 40-character lowercase hexadecimal SHA",
            ));
        }
        let actor = request_actor(params)?;
        // CAD-1218: a board-relayed approval (Publish) answers to the same
        // approver rule as the drawer's Approve, before anything is
        // recorded. The CLI sends no `request_actor` and keeps `operator`.
        let source = if params.get("request_actor").is_some() {
            super::approvals_rpc::approver_source(&self.pm()?.config, &actor)?
        } else {
            actor.clone()
        };
        let _g = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = delivery::load(&self.state_dir)?;
        let rec = all.get_mut(id).ok_or_else(|| not_in_loop(id))?;
        if !matches!(rec.state, State::Passed | State::Ready) {
            return Err(Error::rejected(format!(
                "{id} has no standing PASS or fresh routine readiness to approve (it is {})",
                rec.state.as_str()
            )));
        }
        let routine = rec.state == State::Ready;
        let pr_url = rec.pr.clone().unwrap_or_default();
        let (slug, number) = task_report::parse_pr_url(&pr_url)
            .map_err(|e| Error::rejected(format!("{id} links no pull request ({e})")))?;
        // The compare basis is read fresh, under the lock — whatever
        // this call returns was true now, not at the last sync.
        let read_at = now();
        let gh_bin = self.delivery_gh.to_string_lossy().to_string();
        let view = delivery::pr_view(&gh_bin, &slug, number).map_err(|e| {
            Error::rejected(format!(
                "{id}: reading the PR failed, nothing was approved — {e}"
            ))
        })?;
        let rollup = view["statusCheckRollup"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let live = view["headRefOid"].as_str().unwrap_or_default().to_string();
        if want != live {
            return Err(Error::invalid(
                "head_moved",
                format!(
                    "{id}: the PR head moved since it was shown (shown {}…, now {}…) — re-review before approving",
                    &want[..12],
                    live.chars().take(12).collect::<String>(),
                ),
            ));
        }
        rec.observed = Some(Observed {
            head: live.clone(),
            pr_state: view["state"].as_str().unwrap_or_default().to_string(),
            ci_green: crate::overview::checks_green_pub(&rollup),
            auto_merge: !view["autoMergeRequest"].is_null(),
            additions: view["additions"].as_u64().unwrap_or(0),
            deletions: view["deletions"].as_u64().unwrap_or(0),
            files: view["changedFiles"].as_u64().unwrap_or(0),
            at: now(),
            read_at,
        });
        if routine {
            let report = rec.outcome_report.as_deref().unwrap_or_default();
            match delivery_requirements_rpc::routine_readiness(self, id, &pr_url, &live, report) {
                Ok(RoutineReadiness::Ready(evidence)) => {
                    let binding = delivery::ReadyBinding {
                        repo: evidence.repo.clone(),
                        pr: evidence.pr,
                        head: evidence.head.clone(),
                        base: evidence.base.clone(),
                        base_ref: evidence.base_ref.clone(),
                        policy_digest: evidence.policy_digest.clone(),
                        ci_run_id: evidence.ci_run_id,
                        ci_run_url: evidence.ci_run_url.clone(),
                        outcome_report: evidence.outcome_report.clone(),
                    };
                    if rec.ready_binding.as_ref() != Some(&binding) {
                        rec.ready_binding = None;
                        rec.ready_evidence = None;
                        rec.enter(State::Working, now());
                        rec.disable_auto = rec.observed.as_ref().is_some_and(|o| o.auto_merge);
                        delivery::save(&self.state_dir, &all)?;
                        self.wake();
                        return Err(Error::rejected(format!(
                            "{id} routine readiness binding changed; refresh and re-authorize"
                        )));
                    }
                    rec.ready_binding = Some(binding);
                    rec.ready_evidence = Some(evidence);
                }
                Ok(RoutineReadiness::Waiting | RoutineReadiness::NotRoutine) | Err(_) => {
                    rec.ready_binding = None;
                    rec.ready_evidence = None;
                    rec.enter(State::Working, now());
                    rec.disable_auto = rec.observed.as_ref().is_some_and(|o| o.auto_merge);
                    delivery::save(&self.state_dir, &all)?;
                    self.wake();
                    return Err(Error::rejected(format!(
                        "{id} routine readiness is stale or incomplete; no merge was approved"
                    )));
                }
            }
        }
        if !routine {
            // CAD-1561: the requirements classifier is the authoritative
            // admission of the CURRENT target under the CURRENT approved
            // policy. It runs for EVERY activation here — an
            // unapproved/removed/malformed policy or a base that is
            // neither the repository default nor the approved
            // `integration_branch` refuses even when no profile floor
            // applies — before the approval is recorded and before `gh`
            // enqueues.
            let request = json!({"issue": id, "pr": pr_url, "head": live});
            let current = delivery_requirements_rpc::classify(self, &request).map_err(|_| {
                Error::rejected(
                    "current profile requirements could not be verified; no merge was approved",
                )
            })?;
            let requirements = &current["requirements"];
            if requirements["activation"] == "active" {
                if requirements["reviews"]
                    .as_u64()
                    .is_none_or(|count| count > 1)
                    || requirements["browser_qa"] == true
                {
                    return Err(Error::rejected(
                        "native approval cannot satisfy current review or Browser QA requirements; use the reviewed-enqueue",
                    ));
                }
                if requirements["full_checks"] == true {
                    let base = current["base"].as_str().ok_or_else(|| {
                        Error::rejected("current profile requirements have no base SHA")
                    })?;
                    let base_ref = current["base_ref"].as_str().ok_or_else(|| {
                        Error::rejected("current profile requirements have no base branch name")
                    })?;
                    if !delivery_requirements_rpc::full_ci_green(self, &slug, number, &live, base, base_ref, false)
                        .map_err(|_| Error::rejected("current required checks or canonical ci.yml run could not be verified; no merge was approved"))?
                    {
                        return Err(Error::rejected("current required checks or canonical ci.yml run are not green; no merge was approved"));
                    }
                }
            }
            // Freshness through the side effects below: the same
            // classification is re-collected and every admission input —
            // head, base commit, base branch, policy digest,
            // requirements — must be identical, so a target or policy
            // that changed while the floors ran still refuses.
            let confirmed = delivery_requirements_rpc::classify(self, &request).map_err(|_| {
                Error::rejected(
                    "profile requirements changed during approval; no merge was approved",
                )
            })?;
            if confirmed["head"] != current["head"]
                || confirmed["base"] != current["base"]
                || confirmed["base_ref"] != current["base_ref"]
                || confirmed["policy_digest"] != current["policy_digest"]
                || confirmed["requirements"] != current["requirements"]
            {
                return Err(Error::rejected(
                    "profile requirements changed during approval; no merge was approved",
                ));
            }
        }
        if !rec.merge_ready() {
            let why = match &rec.observed {
                None => "GitHub has not been read for it yet".to_string(),
                Some(o) if o.pr_state != "OPEN" => format!("the PR is {}", o.pr_state),
                Some(_) => "its CI is not green".to_string(),
            };
            return Err(Error::rejected(format!(
                "{id} is not ready to merge: {why}"
            )));
        }
        let sha = live;
        // The approval object first: a failed enqueue retries cleanly
        // (same evidence dedupes to the same id), and audit never sees
        // a merge without its approval.
        // Approvals are scoped by the lowercase slug (what the board's
        // Approve stores); `gh` keeps the PR link's own case.
        let approval_repo = slug.to_ascii_lowercase();
        let approval = store::NewApproval {
            id: None,
            source: &source,
            action: "merge",
            head_sha: &sha,
            repo: &approval_repo,
            pr: number,
        };
        // CAD-1300: a board Publish (it relays `request_actor`) follows the
        // one-record-per-head rule; the CLI keeps `record_approval`.
        let approval_id = if params.get("request_actor").is_some() {
            self.store
                .record_or_reuse_board_approval(&approval, APPROVAL_RECORDED_VIA, id)?
        } else {
            self.store
                .record_approval(&approval, APPROVAL_RECORDED_VIA)?
                .1
        };
        if let Err(e) = delivery::gh(
            &gh_bin,
            &[
                "pr",
                "merge",
                &number.to_string(),
                "-R",
                &slug,
                "--auto",
                "--squash",
                "--match-head-commit",
                &sha,
            ],
        ) {
            return Err(Error::rejected(format!(
                "{id}: the merge did not enqueue ({e}) — approval {approval_id} stands; retry the approval"
            )));
        }
        rec.enter(State::Enqueued, now());
        rec.unattended_hold = false;
        let mut out = rec.to_json();
        out["approval_id"] = serde_json::json!(approval_id);
        out["approved_by"] = serde_json::json!(actor);
        delivery::save(&self.state_dir, &all)?;
        if let Ok(pm) = self.pm() {
            let _ = issue::write::add_comment(
                &pm,
                id,
                &format!(
                    "Merge approved by {actor} and enqueued: {pr_url}, pinned to {sha} (approval {approval_id})."
                ),
                Some("operator"),
                Some("review"),
                None,
                "operator",
            );
        }
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "merge_enqueued",
            serde_json::json!({"issue": id, "sha": sha, "pr": pr_url, "approval_id": approval_id}),
        );
        self.wake();
        Ok(out)
    }

    /// `delivery_decline` — the operator declines the merge decision
    /// (or an escalated or unstaffed review) with a reason.
    pub(super) fn rpc_delivery_decline(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("delivery decline", params, peer_pid)?;
        let id = required_str(params, "issue")?;
        let reason = required_str(params, "reason")?.trim();
        if reason.is_empty() || reason.len() > REASON_MAX {
            return Err(Error::rejected(format!(
                "a decline states its reason (1-{REASON_MAX} bytes)"
            )));
        }
        crate::secret::guard(&format!("{id}: decline"), reason)?;
        // CAD-140: who declined — the record, the comment and the
        // event carry the deciding actor (attribution only; authority
        // is this connection, gated above).
        let actor = request_actor(params)?;
        let _g = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = delivery::load(&self.state_dir)?;
        let rec = all.get_mut(id).ok_or_else(|| not_in_loop(id))?;
        if rec.state.terminal() {
            return Err(Error::rejected(format!(
                "{id} is already {}",
                rec.state.as_str()
            )));
        }
        if rec.state == State::Enqueued || rec.observed.as_ref().is_some_and(|o| o.auto_merge) {
            rec.disable_auto = true;
        }
        rec.note = Some(reason.to_string());
        rec.declined_by = Some(actor.clone());
        rec.enter(State::Declined, now());
        rec.unattended_hold = false;
        let out = rec.to_json();
        let ended = rec.clone();
        delivery::save(&self.state_dir, &all)?;
        self.wake_on_delivery_end(&ended, None);
        if let Ok(pm) = self.pm() {
            let _ = issue::write::add_comment(
                &pm,
                id,
                &format!("Merge declined by {actor}: {reason}"),
                Some("operator"),
                Some("review"),
                None,
                "operator",
            );
        }
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "merge_declined",
            json!({"issue": id, "reason": reason, "actor": actor}),
        );
        self.wake();
        Ok(out)
    }
}

/// An observation worth an event and a wake: the state moved, or
/// auto-merge newly needs turning off. A `disable_auto` that stays
/// true (the operator's `gh` keeps failing to turn it off) wakes once,
/// not on every observation (CAD-446: the board observes every minute).
fn rec_state_changed(out: &Value, was_disable: bool) -> bool {
    out["state"] != out["was"] || (out["disable_auto"] == true && !was_disable)
}

/// Something the loop tells after `delivery_lock` is released: an
/// optional ticket comment and one event.
struct Notice {
    issue: String,
    comment: Option<String>,
    kind: &'static str,
    payload: Value,
}

fn rec_pr_ref(rec: &Record) -> String {
    rec.pr
        .as_deref()
        .and_then(delivery::pr_ref)
        .unwrap_or_default()
}
