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
//!
//! Every refusal happens before anything is written — the tracker, the
//! record and the message queue alike.

use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{optional_str, required_str, Shared, DAEMON_ALIAS};
use crate::delivery::{self, Candidate, Observed, Record, State, VerdictRec};
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
        let _g = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = delivery::load(&self.state_dir)?;
        if all.get(id).is_some_and(|r| !r.state.terminal()) {
            return Ok(());
        }
        all.insert(id.to_string(), Record::new(id, project, worker, now()));
        delivery::save(&self.state_dir, &all)
    }

    /// One router pass over the loop (CAD-431): the worker's newest
    /// unconsumed `done` report goes to review; an unstaffed review is
    /// retried. Returns how many records moved.
    pub(super) fn route_delivery(self: &Arc<Self>) -> Result<usize> {
        let _g = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = delivery::load(&self.state_dir)?;
        if all.values().all(|r| r.state.terminal()) {
            return Ok(0);
        }
        let pm = Pm::at(&self.pm_dir()?)?;
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
        let Some(key) = delivery::pr_ref(pr) else {
            return Ok(Some(format!(
                "names `pr: {pr}`, which is not a pull request URL"
            )));
        };
        let (slug, _) = task_report::parse_pr_url(pr)?;
        let want = crate::issue::project::normalize_remote(&format!("github.com/{slug}"))
            .to_ascii_lowercase();
        let project = crate::issue::project::list(&pm.dir)?
            .into_iter()
            .find(|p| p.key == rec.project);
        let remotes: Vec<String> = project
            .iter()
            .flat_map(|p| p.repos.iter())
            .filter_map(|r| r.remote.as_deref())
            .map(|r| crate::issue::project::normalize_remote(r).to_ascii_lowercase())
            .collect();
        if !remotes.contains(&want) {
            let listed = if remotes.is_empty() {
                "none — `repos[].remote` is unset".to_string()
            } else {
                remotes.join(", ")
            };
            return Ok(Some(format!(
                "names {key}, which is not a repo of project {} (its remotes: {listed})",
                rec.project
            )));
        }
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
        let pm = Pm::at(&self.pm_dir()?)?;
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
    pub(super) fn rpc_delivery_list(&self, params: &Value) -> Result<Value> {
        let only = optional_str(params, "issue");
        let mut rows: Vec<Record> = delivery::load(&self.state_dir)?
            .into_values()
            .filter(|r| only.is_none_or(|o| o == r.issue))
            .collect();
        rows.sort_by_key(|r| r.dispatched_at);
        Ok(json!({"records": rows.iter().map(Record::to_json).collect::<Vec<_>>()}))
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
        let pm = Pm::at(&self.pm_dir()?)?;
        let _g = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
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
        let approved = rec.state == State::Enqueued && rec.passed_sha() == Some(head.as_str());
        rec.disable_auto = obs.auto_merge && !approved && rec.state != State::Merged;
        rec.observed = Some(obs);
        let out = json!({
            "issue": id, "state": rec.state.as_str(), "was": before.as_str(),
            "disable_auto": rec.disable_auto, "merge_ready": rec.merge_ready(),
        });
        delivery::save(&self.state_dir, &all)?;
        if rec_state_changed(&out) {
            let _ = self
                .store
                .event_public(DAEMON_ALIAS, "delivery_observed", out.clone());
            self.wake();
        }
        Ok(out)
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
                if let Ok(pm) = self.pm_dir().and_then(|d| Pm::at(&d)) {
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
    pub(super) fn rpc_delivery_decline(&self, params: &Value, peer_pid: u32) -> Result<Value> {
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
        delivery::save(&self.state_dir, &all)?;
        if let Ok(pm) = self.pm_dir().and_then(|d| Pm::at(&d)) {
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

fn rec_state_changed(out: &Value) -> bool {
    out["state"] != out["was"] || out["disable_auto"] == true
}
