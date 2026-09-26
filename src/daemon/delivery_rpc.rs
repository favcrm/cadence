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

use super::{check_values, optional_str, optional_strs, required_str, Shared, DAEMON_ALIAS};
use crate::delivery::{self, Candidate, Observed, Record, State, TicketDone, VerdictRec};
use crate::error::{Error, Result};
use crate::issue::{self, task_report, Pm};
use crate::master;
use crate::peer::AgentCaller;
use crate::store;

/// Longest decline reason.
const REASON_MAX: usize = 2_000;

fn now() -> i64 {
    crate::issue::time::now_epoch()
}

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
        self.delivery_adopt(id, project, worker, now())
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
        let _g = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = delivery::load(&self.state_dir)?;
        if all.values().all(|r| r.state.terminal()) {
            return Ok(0);
        }
        let pm = self.pm()?;
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
            match self.route_record(&pm, rec, &held) {
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
    ) -> Result<bool> {
        if rec.state.terminal() || rec.state == State::Escalated {
            return Ok(false);
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
            if rec.state == State::Unstaffed {
                self.start_review(pm, rec)?;
                return Ok(rec.state != State::Unstaffed);
            }
            return Ok(false);
        };
        let name = latest["name"].as_str().unwrap_or_default();
        let refusal = match (latest["sha"].as_str(), latest["pr"].as_str()) {
            (Some(sha), Some(pr)) => match self.pr_refusal(pm, rec, pr, held)? {
                None => {
                    self.on_done(pm, rec, sha, pr)?;
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
    /// dispatched.
    fn pr_refusal(
        &self,
        pm: &Pm,
        rec: &Record,
        pr: &str,
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
        Ok(None)
    }

    /// The worker reported `sha` done on `pr`.
    fn on_done(self: &Arc<Self>, pm: &Pm, rec: &mut Record, sha: &str, pr: &str) -> Result<()> {
        let same_head = rec.head.as_deref() == Some(sha) && rec.pr.as_deref() == Some(pr);
        if same_head
            && matches!(
                rec.state,
                State::Reviewing | State::Passed | State::Enqueued
            )
        {
            return Ok(());
        }
        // A new head after review: whatever auto-merge is on was set for
        // the old head.
        if matches!(rec.state, State::Passed | State::Enqueued)
            && (rec.state == State::Enqueued || rec.observed.as_ref().is_some_and(|o| o.auto_merge))
        {
            rec.disable_auto = true;
        }
        rec.pr = Some(pr.to_string());
        rec.head = Some(sha.to_string());
        self.start_review(pm, rec)
    }

    /// The head moved without a done report from the worker: whoever
    /// reviewed it may have pushed it, so that reviewer is barred from
    /// this ticket and the review goes to someone else.
    fn review_moved_head(self: &Arc<Self>, pm: &Pm, rec: &mut Record) -> Result<()> {
        if let Some(prev) = rec.reviewer.take() {
            if !rec.excluded.contains(&prev) {
                rec.excluded.push(prev);
            }
        }
        self.start_review(pm, rec)
    }

    /// Route a review of `rec.head` to an independent reviewer, or mark
    /// the record unstaffed when nobody qualifies.
    fn start_review(self: &Arc<Self>, pm: &Pm, rec: &mut Record) -> Result<()> {
        let (Some(sha), Some(pr)) = (rec.head.clone(), rec.pr.clone()) else {
            return Ok(());
        };
        let agents = self.store.agents()?;
        let worker_provider = agents
            .iter()
            .find(|a| a.alias == rec.worker)
            .map(|a| a.provider.clone());
        let candidates: Vec<Candidate> = agents
            .iter()
            .map(|a| Candidate {
                alias: a.alias.clone(),
                provider: a.provider.clone(),
                state: a.state.clone(),
                enabled: a.enabled,
                upstream: super::agent_upstream(a).map(str::to_string),
            })
            .collect();
        let Some(reviewer) = delivery::pick_reviewer(
            &rec.worker,
            worker_provider.as_deref(),
            rec.reviewer.as_deref(),
            &rec.excluded,
            &candidates,
        ) else {
            if rec.state != State::Unstaffed {
                rec.enter(State::Unstaffed, now());
                rec.note = Some(format!(
                    "no agent can review {sha}: none is registered besides the worker {} and \
                     the master, or every other one is fenced",
                    rec.worker
                ));
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "review_unstaffed",
                    json!({"issue": rec.issue, "sha": sha}),
                );
            }
            return Ok(());
        };
        let agent = agents
            .iter()
            .find(|a| a.alias == reviewer)
            .ok_or_else(|| Error::internal("reviewer vanished"))?;
        let ticket = issue::board::find_issue(&pm.dir, &rec.issue)?;
        let items = issue::parse::acceptance_items(&ticket.body);
        let listing = issue::dispatch::acceptance_listing(&items);
        let round = rec.rounds + 1;
        let text = delivery::review_kickoff(
            &rec.issue,
            round,
            &pr,
            &sha,
            &rec.worker,
            listing.as_deref(),
            &ticket.dir.join("issue.md"),
            store::kickoff_ceiling(&agent.provider, &agent.endpoint_kind),
        );
        let mid = format!("review-{}", hash(&format!("{}/{sha}/{round}", rec.issue)));
        self.send_as(
            &json!({"alias": reviewer, "text": text, "message": mid, "source": "review"}),
            &|_| Ok(store::Sender::Unattributed),
        )?;
        rec.rounds = round;
        rec.reviewer = Some(reviewer.clone());
        rec.note = None;
        rec.enter(State::Reviewing, now());
        let _ = issue::write::add_comment(
            pm,
            &rec.issue,
            &format!("Review round {round} routed to {reviewer}: {pr} at {sha}."),
            Some(DAEMON_ALIAS),
            Some("review"),
            None,
            DAEMON_ALIAS,
        );
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "review_routed",
            json!({"issue": rec.issue, "reviewer": reviewer, "sha": sha, "pr": pr,
                   "round": round, "message": mid}),
        );
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
                )))
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
        let head = rec.head.clone().unwrap_or_default();
        if sha != head {
            return Err(Error::rejected(format!(
                "stale verdict: it judges {sha}, but the head under review is {head} — \
                 review {head} and file the verdict for it"
            )));
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
        let comment = match verdict {
            task_report::Verdict::Pass => {
                rec.enter(State::Passed, at);
                format!("PASS by {who} at {sha}: {summary}")
            }
            task_report::Verdict::Revise => {
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
                    let msg = delivery::revise_message(
                        id,
                        &who,
                        &sha,
                        rec.revisions,
                        &report,
                        &summary,
                        rec.pr.as_deref().unwrap_or_default(),
                    );
                    self.send_as(
                        &json!({"alias": rec.worker, "text": msg,
                                "message": format!("revise-{}", hash(&report)),
                                "source": "review"}),
                        &|_| Ok(store::Sender::Unattributed),
                    )?;
                    rec.enter(State::Working, at);
                    format!(
                        "REVISE {} of {} by {who} at {sha}: {summary} — back to {}",
                        rec.revisions,
                        delivery::MAX_REVISE,
                        rec.worker
                    )
                }
            }
        };
        let out = rec.to_json();
        delivery::save(&self.state_dir, &all)?;
        // CAD-449: the daemon's own statement of this verdict — what a
        // merge must match to mark the ticket done.
        if let Err(e) = self.store.record_review_verdict(json!({
            "issue": id, "verdict": verdict.as_str(), "sha": sha, "reviewer": who,
            "report": report,
        })) {
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
        };
        let pm = self.pm()?;
        let guard = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = delivery::load(&self.state_dir)?;
        let rec = all.get_mut(id).ok_or_else(|| not_in_loop(id))?;
        let before = rec.state;
        let moved = rec.head.as_deref() != Some(head.as_str());
        if !rec.state.terminal() {
            match pr_state {
                "MERGED" => rec.enter(State::Merged, obs.at),
                "CLOSED" => rec.enter(State::Closed, obs.at),
                _ if moved
                    && matches!(
                        rec.state,
                        State::Reviewing | State::Passed | State::Enqueued
                    ) =>
                {
                    rec.head = Some(head.clone());
                    self.review_moved_head(&pm, rec)?;
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
        // Auto-merge stays on only for the enqueued, reviewed head.
        let was_disable = rec.disable_auto;
        let approved = rec.state == State::Enqueued && rec.passed_sha() == Some(head.as_str());
        rec.disable_auto = obs.auto_merge && !approved && rec.state != State::Merged;
        rec.observed = Some(obs);
        let mut out = json!({
            "issue": id, "state": rec.state.as_str(), "was": before.as_str(),
            "disable_auto": rec.disable_auto, "merge_ready": rec.merge_ready(),
        });
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
        // The wake first: it releases `wake_lock`, which a comment waiting
        // on the tracker lock must not hold.
        if let Some(rec) = ended {
            self.wake_on_delivery_end(&rec, wake_guard);
        } else {
            drop(wake_guard);
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
    /// hook) is written again; `refused` becomes `kept` once the
    /// operator set the status by hand. Answers how many records moved.
    pub(super) fn settle_ticket_done(&self) -> Result<usize> {
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
            for id in open {
                let Some(rec) = all.get_mut(&id) else {
                    continue;
                };
                let next = match rec.ticket_done.clone() {
                    Some(TicketDone::Pending { why, from }) => {
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
                        let head = rec.observed.as_ref().map(|o| o.head.clone());
                        let mut mine = Vec::new();
                        let next = self.mark_ticket_done(
                            &pm,
                            rec,
                            &head.unwrap_or_default(),
                            Some(&from),
                            &mut mine,
                        );
                        // One comment per failure, not one per pass.
                        if matches!(&next, TicketDone::Pending { why: now, .. } if *now == why) {
                            continue;
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
                moved += 1;
            }
            if moved > 0 {
                delivery::save(&self.state_dir, &all)?;
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
        match issue::write::mark_done_on_merge(pm, id, &why, &actor, expect) {
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
                let why = e.to_string();
                notices.push(Notice {
                    issue: id.clone(),
                    comment: Some(format!(
                        "{pr_ref} merged at {head}, but marking {id} done failed: {why}. \
                         The daemon retries; the operator can set its status."
                    )),
                    kind: "ticket_done_pending",
                    payload: json!({"issue": id, "pr": pr_ref, "head": head, "why": why}),
                });
                // What a retry must still find: the status the write
                // left in place (it rolled back), read now.
                let from = match expect {
                    Some(e) => Some(e.to_string()),
                    None => issue::board::find_issue(&pm.dir, id)
                        .ok()
                        .map(|t| t.front.status),
                };
                TicketDone::Pending { why, from }
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
            let busy = n.kind == "ticket_done_pending" && pm.dir.join(".write.lock").exists();
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
        if let Some(why) = self.pr_refusal(pm, rec, pr, &[])? {
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
        if rec.state != State::Passed {
            return Err(Error::rejected(format!(
                "{id} has no standing PASS to merge (it is {})",
                rec.state.as_str()
            )));
        }
        let sha = rec.passed_sha().unwrap_or_default().to_string();
        let pr = rec.pr.clone().unwrap_or_default();
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
                if phase == "check" {
                    return Ok(json!({"issue": id, "sha": sha, "pr": pr}));
                }
                if optional_str(params, "sha") != Some(sha.as_str()) {
                    return Err(Error::rejected(format!(
                        "{id}'s reviewed head is {sha} — the enqueue must be pinned to it"
                    )));
                }
                rec.enter(State::Enqueued, now());
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
        rec.enter(State::Declined, now());
        let out = rec.to_json();
        let ended = rec.clone();
        delivery::save(&self.state_dir, &all)?;
        self.wake_on_delivery_end(&ended, None);
        if let Ok(pm) = self.pm() {
            let _ = issue::write::add_comment(
                &pm,
                id,
                &format!("Merge declined by the operator: {reason}"),
                Some("operator"),
                Some("review"),
                None,
                "operator",
            );
        }
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "merge_declined",
            json!({"issue": id, "reason": reason}),
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
