//! CAD-484 — the idle lane's one next action. The checkup's report
//! walk collects every `done` report ([`DoneMap`]); when it visits an
//! agent that holds no running or queued turn, the lane's open work
//! says what its one next step is:
//!
//! - **fix**: its open PR carries a REVISE verdict on the head under
//!   review, or the operator's `gh` observation saw red CI on that
//!   head — one fix turn to the lane, once per head
//!   (`checkup-fix-<issue>-<sha>`); if it is still idle after that
//!   turn, a person gets the Needs-you row instead of a second paste.
//! - **review**: the head has nobody reviewing it — the delivery
//!   loop's own routing runs now and assigns one free reviewer. A
//!   lane the master never dispatched is adopted into the loop from
//!   its done report (`sha:` + `pr:`), so a CLI dispatch gets the
//!   same review.
//! - **dispatch**: the PR waits on a person — the merge decision, an
//!   operator's acceptance, an escalated review — or nothing at all
//!   stands pending on the lane. One `ready` ticket whose declared
//!   `paths` share nothing with an open PR, an in-flight lane, or an
//!   operator-acceptance ticket goes to the lane through the ordinary
//!   dispatch path; the waiting PR is untouched.
//! - **needs-you**: nothing above is safe — one escalation row while
//!   the lane stays idle.
//!
//! What it never does: merge, restart a provider, or act on a busy
//! lane. A fix turn is a daemon `source: "review"` message like the
//! loop's own hand-backs; a dispatch reuses `issue::dispatch::run`
//! under `dispatch_lock`, so the ticket's `ready` check, its claim,
//! its refs and its kickoff are the ones every dispatch makes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::json;

use super::checkup::{Outcome, Pane, CHECKUP_EVENT};
use super::{agent_upstream, Shared, DAEMON_ALIAS};
use crate::delivery::{self, State};
use crate::error::{Error, Result};
use crate::issue::board::{self, Issue};
use crate::issue::{self, areas, Pm};
use crate::master;
use crate::store::{self, Agent};

/// A `done` report one agent filed — the lane's claim on the issue —
/// collected by the checkup's report walk.
#[derive(Debug)]
pub(super) struct DoneRef {
    pub issue: String,
    pub project: String,
    /// The report's file name — a delivery record's `handled` key.
    pub name: String,
    /// When it was filed (epoch seconds).
    pub at: i64,
    pub sha: Option<String>,
    pub pr: Option<String>,
    /// The issue's tracker status at the walk.
    pub status: String,
}

/// Agent alias → the `done` reports it filed anywhere in the tracker.
pub(super) type DoneMap = HashMap<String, Vec<DoneRef>>;

impl Shared {
    /// An agent holding no running or queued turn is still visited
    /// when it has reported done: the lane's one next action — fix,
    /// review, dispatch or a Needs-you row — is the visit's outcome,
    /// written on the same `checkup` event as the turn-holding path.
    pub(super) fn checkup_idle_lane(
        self: &Arc<Self>,
        agent: &Agent,
        reports: &[DoneRef],
    ) -> Result<()> {
        let (outcome, reason) = self.idle_lane_decide(agent, reports)?;
        let _ = self.store.event_public(
            &agent.alias,
            CHECKUP_EVENT,
            json!({
                "outcome": outcome.as_str(),
                "running": false,
                "reason": reason,
            }),
        );
        Ok(())
    }

    /// The lane's one next step. It earns an act only when it is
    /// provably at rest: state `idle`, enabled, and — for a pty lane —
    /// not visibly busy. Anything else is left alone.
    fn idle_lane_decide(
        self: &Arc<Self>,
        agent: &Agent,
        reports: &[DoneRef],
    ) -> Result<(Outcome, String)> {
        if agent.state != "idle" {
            return Ok((Outcome::Leave, format!("not idle — it is {}", agent.state)));
        }
        if !agent.enabled {
            return Ok((Outcome::Leave, "agent disabled".to_string()));
        }
        if agent.endpoint_kind == "pty" && self.pane_state(agent) == Pane::Busy {
            return Ok((
                Outcome::Leave,
                "pane busy — the lane is left alone".to_string(),
            ));
        }
        let records = delivery::load(&self.state_dir)?;
        // One subject per issue: the lane's newest done report on it.
        let mut latest: HashMap<&str, &DoneRef> = HashMap::new();
        for rep in reports {
            let slot = latest.entry(rep.issue.as_str()).or_insert(rep);
            if rep.at >= slot.at {
                *slot = rep;
            }
        }
        let mut subjects: Vec<&DoneRef> = latest.into_values().collect();
        subjects.sort_by(|a, b| {
            b.at.cmp(&a.at).then_with(|| {
                delivery::filing_order(&b.name, &agent.alias)
                    .cmp(&delivery::filing_order(&a.name, &agent.alias))
            })
        });
        // A subject whose PR waits on a person licenses the dispatch;
        // one still in the loop's flight does not.
        let mut waiting = false;
        let mut pending = false;
        for rep in &subjects {
            if rep.status == "dropped" {
                continue;
            }
            match records.get(&rep.issue) {
                // The loop already judged it — the lane is free.
                Some(r) if r.state.terminal() => {}
                // Another worker holds the record — not this lane's
                // report to act on.
                Some(r) if r.worker != agent.alias => {}
                Some(r) => {
                    if let Some(done) = self.subject_action(agent, rep, r)? {
                        return Ok(done);
                    }
                    if matches!(r.state, State::Passed | State::Enqueued | State::Escalated) {
                        waiting = true;
                    } else {
                        pending = true;
                    }
                }
                // Never in the loop: adopt it when the report names a
                // reviewable head and the ticket still stands open.
                // A `done` issue's unrecorded PR is the operator's to
                // wonder at, not a lane to staff.
                None if rep.pr.is_some() && rep.sha.is_some() && rep.status != "done" => {
                    return self.adopt_and_route(agent, rep);
                }
                None => {}
            }
        }
        if pending && !waiting {
            return Ok((
                Outcome::Leave,
                "its work is in flight — the review loop owns it".to_string(),
            ));
        }
        self.dispatch_or_flag(agent, subjects.first().map(|r| r.issue.as_str()))
    }

    /// One live subject: act and answer its outcome, or `None` when
    /// nothing on it is due.
    fn subject_action(
        self: &Arc<Self>,
        agent: &Agent,
        rep: &DoneRef,
        r: &delivery::Record,
    ) -> Result<Option<(Outcome, String)>> {
        let head = r.head.as_deref().unwrap_or_default();
        let revise = r
            .verdict
            .as_ref()
            .is_some_and(|v| v.verdict == "revise" && v.sha == head);
        let red_ci = r.observed.as_ref().is_some_and(|o| {
            o.pr_state == "OPEN" && !o.ci_green && !head.is_empty() && o.head == head
        });
        if (revise || red_ci) && !head.is_empty() {
            return Ok(Some(self.fix_turn(agent, r, revise)?));
        }
        // The lane's newest done report is unconsumed, or a review is
        // due and nobody holds it — the loop's own routing runs now.
        // An unparseable report routes too: the router's refusal tell
        // is how the worker learns `sha:`/`pr:` were missing.
        let fresh = !r.handled.iter().any(|h| h == &rep.name);
        if fresh || (r.state == State::Unstaffed && r.pr.is_some()) {
            return Ok(Some(self.route_now(&rep.issue)?));
        }
        // A record that consumed every done report but never staffed a
        // review cannot re-enter the loop on its own — a person looks.
        if r.state == State::Working && r.reviewer.is_none() && r.head.is_some() && !fresh {
            self.flag_lane(
                agent,
                &rep.issue,
                "the done report was handled but no review stands — the loop lost it",
            )?;
            return Ok(Some((
                Outcome::Escalate,
                "record stuck mid-route — Needs-you row".to_string(),
            )));
        }
        Ok(None)
    }

    /// A lane the master never dispatched still earned its review:
    /// the record opens at the done report's own time — the report
    /// predates the loop — then the routing pass runs.
    fn adopt_and_route(
        self: &Arc<Self>,
        agent: &Agent,
        rep: &DoneRef,
    ) -> Result<(Outcome, String)> {
        self.delivery_adopt(&rep.issue, &rep.project, &agent.alias, rep.at)?;
        self.route_now(&rep.issue)
    }

    /// Route the record's outstanding done report now and answer with
    /// the state it landed in.
    fn route_now(self: &Arc<Self>, issue: &str) -> Result<(Outcome, String)> {
        self.route_delivery()?;
        let Some(rec) = delivery::load(&self.state_dir)?.remove(issue) else {
            return Ok((Outcome::Leave, "the record vanished mid-route".to_string()));
        };
        Ok(match rec.state {
            State::Reviewing => (
                Outcome::Review,
                format!(
                    "review round {} routed to {}",
                    rec.rounds,
                    rec.reviewer.as_deref().unwrap_or("?")
                ),
            ),
            State::Unstaffed => (
                Outcome::Escalate,
                "no free reviewer — the unstaffed row stands".to_string(),
            ),
            _ => (
                Outcome::Leave,
                "the done report was refused — the lane was told".to_string(),
            ),
        })
    }

    /// One fix turn on the lane's PR: the REVISE hand-back again, or a
    /// red-CI note — once per head. Still idle after it, the lane is a
    /// person's, not a second paste's.
    fn fix_turn(
        self: &Arc<Self>,
        agent: &Agent,
        r: &delivery::Record,
        revise: bool,
    ) -> Result<(Outcome, String)> {
        let head = r.head.clone().unwrap_or_default();
        // Message ids are lowercase-only — the issue id is not.
        let mid = format!(
            "checkup-fix-{}-{}",
            r.issue.to_lowercase(),
            &head[..head.len().min(12)]
        );
        if self.store.message(&mid)?.is_some() {
            self.flag_lane(
                agent,
                &r.issue,
                &format!("a fix turn for {head} already went out and the lane is idle again"),
            )?;
            return Ok((
                Outcome::Escalate,
                "fix already sent — the lane did not move".to_string(),
            ));
        }
        let pr = r.pr.as_deref().unwrap_or_default();
        let text = match r.verdict.as_ref().filter(|_| revise) {
            Some(v) => delivery::revise_message(
                &r.issue,
                &v.reviewer,
                &v.sha,
                r.revisions,
                &v.report,
                &v.summary,
                pr,
            ),
            None => format!(
                "[fix] {}: CI is failing on {} at {} — read the checks (`gh pr checks`), \
                 fix it on the same branch and push, then file `cadence report file \
                 --task {} --kind done` with `sha: <new head>` and `pr: {}`; the new \
                 head goes back to review.",
                r.issue, pr, head, r.issue, pr
            ),
        };
        self.send_as(
            &json!({"alias": agent.alias, "text": text, "message": mid,
                    "source": "review"}),
            &|_| Ok(store::Sender::Unattributed),
        )?;
        let why = match (revise, r.observed.as_ref().is_some_and(|o| !o.ci_green)) {
            (true, true) => "a revise verdict and red CI on the head",
            (true, false) => "a revise verdict on the head",
            _ => "red CI on the head",
        };
        Ok((Outcome::Fix, format!("one fix turn sent — {why}")))
    }

    /// The lane is free: dispatch one ready ticket that shares no
    /// declared or committed path with an open PR, an in-flight lane,
    /// or a ticket done-but-open-for-acceptance. No safe candidate is
    /// one Needs-you row.
    fn dispatch_or_flag(
        self: &Arc<Self>,
        agent: &Agent,
        subject: Option<&str>,
    ) -> Result<(Outcome, String)> {
        let pm = self.pm()?;
        let issues = board::load_all(&pm.dir, None)?;
        let records = delivery::load(&self.state_dir)?;
        let lanes = areas::dispatches(&self.state_dir);
        // What every standing lane already touches: its declared
        // `paths` plus what its worktree actually committed. A lane the
        // dispatch record binds is probed at its recorded branch; an
        // unbound lane at its open frontmatter worktree.
        let mut claimed: Vec<(String, Vec<String>)> = Vec::new();
        for i in &issues {
            let f = &i.front;
            let open_pr = f
                .refs
                .iter()
                .any(|r| r.kind == "pr" && r.closed != Some(true))
                || records
                    .get(&f.id)
                    .is_some_and(|r| !r.state.terminal() && r.pr.is_some());
            let in_flight = matches!(f.status.as_str(), "doing" | "review")
                || f.refs
                    .iter()
                    .any(|r| r.kind == "worktree" && r.closed != Some(true));
            // `open_pr` covers a `done` issue whose PR is still open —
            // the operator-acceptance lane.
            if !(open_pr || in_flight) {
                continue;
            }
            // Planted frontmatter bypasses `issue set`'s check — take
            // only paths a write would accept, as `open_lanes` does.
            let mut files: Vec<String> = f
                .paths
                .iter()
                .filter(|p| areas::check_path(p).is_ok())
                .cloned()
                .collect();
            let probe = lanes
                .get(&f.id)
                .map(|r| {
                    (
                        r["worktree"].as_str().unwrap_or_default().to_string(),
                        r["branch"].as_str().unwrap_or("HEAD").to_string(),
                    )
                })
                .or_else(|| {
                    f.refs
                        .iter()
                        .find(|r| r.kind == "worktree" && r.closed != Some(true))
                        .and_then(|r| r.path.clone())
                        .map(|wt| (wt, "HEAD".to_string()))
                });
            if let Some((dir, rev)) = probe {
                if !dir.is_empty() {
                    if let Ok(changed) = areas::changed_files(Path::new(&dir), &rev) {
                        files.extend(changed);
                    }
                }
            }
            claimed.push((f.id.clone(), files));
        }
        // The safe pick: ready, unblocked, unowned or ours, gated the
        // way `cadence dispatch` gates it, with declared paths that
        // reach nothing a standing lane touches.
        let mut candidates: Vec<&Issue> = issues
            .iter()
            .filter(|i| {
                let f = &i.front;
                // The pick must carry declared paths — without them no
                // overlap check can prove the dispatch safe — and every
                // one of them must parse.
                f.status == "ready"
                    && f.owner.as_deref().is_none_or(|o| o == agent.alias)
                    && !f.paths.is_empty()
                    && f.paths.iter().all(|p| areas::check_path(p).is_ok())
                    && issue::plan::gate(&pm.dir, f, &i.body).is_ok()
            })
            .collect();
        let rank = |i: &Issue| {
            crate::issue::model::PRIORITIES
                .iter()
                .position(|p| *p == i.front.priority)
                .unwrap_or(usize::MAX)
        };
        candidates.sort_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then_with(|| a.front.created.cmp(&b.front.created))
                .then_with(|| a.front.id.cmp(&b.front.id))
        });
        for cand in candidates {
            let blocked = cand.front.blocked_by.iter().any(|b| {
                !issues
                    .iter()
                    .find(|i| &i.front.id == b)
                    .is_some_and(|t| matches!(t.front.status.as_str(), "done" | "dropped"))
            });
            if blocked {
                continue;
            }
            let overlap = claimed.iter().any(|(id, files)| {
                id != &cand.front.id
                    && cand
                        .front
                        .paths
                        .iter()
                        .any(|p| files.iter().any(|f| areas::overlaps(p, f)))
            });
            if overlap {
                continue;
            }
            // The pick was safe; the dispatch itself may still refuse —
            // a pane parked outside the project, a raced claim. That is
            // one Needs-you row too, keyed on the candidate, and the
            // next pass retries the pick anyway.
            return match self.dispatch_one(&pm, agent, &cand.front.id, &cand.project) {
                Ok(done) => Ok(done),
                Err(e) => {
                    self.flag_lane(
                        agent,
                        &cand.front.id,
                        &format!("the safe pick would not dispatch — {e}"),
                    )?;
                    Ok((
                        Outcome::Escalate,
                        format!(
                            "{} refused to dispatch — Needs-you row ({e})",
                            cand.front.id
                        ),
                    ))
                }
            };
        }
        self.flag_lane(
            agent,
            subject.unwrap_or("?"),
            "no ready ticket shares no files with the standing lanes — a person picks the next step",
        )?;
        Ok((
            Outcome::Escalate,
            "no safe candidate — Needs-you row; the lane stays idle".to_string(),
        ))
    }

    /// The ordinary dispatch under `dispatch_lock`, rechecked inside
    /// it: the ticket may have left `ready` since the pick, and the
    /// loop's record must be writable before anything goes out — the
    /// same guard `master_dispatch` runs.
    fn dispatch_one(
        self: &Arc<Self>,
        pm: &Pm,
        agent: &Agent,
        id: &str,
        project: &str,
    ) -> Result<(Outcome, String)> {
        let _serial = self.dispatch_lock.lock().unwrap_or_else(|e| e.into_inner());
        delivery::load(&self.state_dir).map_err(|e| {
            Error::invalid(
                "delivery_unreadable",
                format!("{e} — the operator repairs it before anything dispatches"),
            )
        })?;
        let ticket = board::find_issue(&pm.dir, id)?;
        let front = &ticket.front;
        if front.status != "ready" {
            return Ok((
                Outcome::Leave,
                format!("{id} left ready before the dispatch — next checkup re-picks"),
            ));
        }
        for blocker in &front.blocked_by {
            let b = board::find_issue(&pm.dir, blocker)?;
            if !matches!(b.front.status.as_str(), "done" | "dropped") {
                return Ok((
                    Outcome::Leave,
                    format!(
                        "{id}'s blocker {blocker} is {} — next checkup re-picks",
                        b.front.status
                    ),
                ));
            }
        }
        if let Some(owner) = front.owner.as_deref() {
            if owner != agent.alias {
                return Ok((
                    Outcome::Leave,
                    format!(
                        "{id} is assigned to {owner} — not {}'s to take",
                        agent.alias
                    ),
                ));
            }
        }
        let target = self.store.agent(&agent.alias)?;
        if target.state == "attention" {
            return Ok((
                Outcome::Leave,
                format!("{} is fenced — nothing goes to it", agent.alias),
            ));
        }
        // The lane picked up work between the visit and the lock —
        // its ticket waits for a free lane, not for this one.
        if target.state != "idle"
            || self.store.running_message(&agent.alias)?.is_some()
            || self.store.queued_head(&agent.alias)?.is_some()
        {
            return Ok((
                Outcome::Leave,
                format!("{} went busy between the check and the lock", agent.alias),
            ));
        }
        let upstream = agent_upstream(agent).unwrap_or(DAEMON_ALIAS);
        let out = match &self.checkup_dispatch {
            Some(dispatch) => dispatch(self, &pm.dir, id, &agent.alias)?,
            None => issue::dispatch::run(
                pm,
                id,
                &issue::dispatch::DispatchArgs {
                    to: agent.alias.clone(),
                    note: None,
                    name: None,
                    base: None,
                    repo: None,
                    reply_to: Some(upstream.to_string()),
                    summary: None,
                    job_spec: None,
                    no_lessons: false,
                    force: false,
                    take_over: None,
                },
                DAEMON_ALIAS,
                &self.state_dir,
                Some(upstream),
            )?,
        };
        // A duplicate answers with the live kickoff somebody else sent.
        if out["dispatched"] == json!(false) {
            return Ok((
                Outcome::Leave,
                format!("{id}'s kickoff is already in flight — nothing sent twice"),
            ));
        }
        if let Err(e) = self.delivery_start(id, project, &agent.alias) {
            tracing::warn!("delivery record for {id}: {e}");
        }
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "checkup_dispatched",
            json!({"issue": id, "to": agent.alias, "message": out["message"]}),
        );
        Ok((
            Outcome::Dispatch,
            format!("dispatched {id} — the lane takes it"),
        ))
    }

    /// One Needs-you row for the lane: the escalation record keyed
    /// `next-action` on its subject issue, written once — a pass that
    /// finds it there already records the same outcome, not a second
    /// row.
    fn flag_lane(&self, agent: &Agent, issue: &str, note: &str) -> Result<()> {
        let key = format!("{issue}/next-action");
        {
            let _guard = self
                .escalation_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !master::escalations(&self.state_dir).contains_key(&key) {
                master::record_escalation(
                    &self.state_dir,
                    &key,
                    json!({
                        "issue": issue, "kind": "next_action",
                        "agent": agent.alias, "summary": note,
                        "by": "checkup",
                        "at": crate::issue::time::iso(crate::issue::time::now_epoch()),
                    }),
                )?;
            }
        }
        self.wake();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Probe;
    use crate::daemon::{AgentCtl, Notify, ServeOptions, StallWatch};
    use crate::delivery::{Observed, Record, VerdictRec};
    use crate::store::{NewAgent, Take};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Dispatch calls the seam records — `(issue, to)`.
    type Dispatches = Arc<Mutex<Vec<(String, String)>>>;

    const SHA1: &str = "1111111111111111111111111111111111111111";
    const PR1: &str = "https://github.com/acme/app/pull/7";

    fn shared() -> (tempfile::TempDir, Arc<Shared>, Dispatches) {
        let dir = tempfile::TempDir::new().unwrap();
        let calls: Dispatches = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&calls);
        let s = Shared::new(
            dir.path(),
            &ServeOptions {
                checkup_dispatch: Some(Arc::new(move |_s, _pm, id, to| {
                    seen.lock().unwrap().push((id.to_string(), to.to_string()));
                    Ok(json!({
                        "dispatched": format!("d-{id}"),
                        "message": format!("d-{id}"),
                    }))
                })),
                ..ServeOptions::default()
            },
        )
        .unwrap();
        (dir, s, calls)
    }

    fn worker(s: &Arc<Shared>, alias: &str, endpoint_kind: &str) {
        staff(s, alias, endpoint_kind, "worker", "idle", None, None);
    }

    /// Register one agent. `team_role` is the model-lookup key and must
    /// not, by itself, make the agent a delivery reviewer.
    fn staff(
        s: &Arc<Shared>,
        alias: &str,
        endpoint_kind: &str,
        role: &str,
        state: &str,
        params: Option<&str>,
        team_role: Option<&str>,
    ) {
        s.store
            .register_agent(&NewAgent {
                alias,
                provider: "fake",
                endpoint_kind,
                role,
                cwd: "/tmp",
                sandbox: "read-only",
                instructions: None,
                params,
                team_role,
                model_policy: None,
            })
            .unwrap();
        s.store.set_agent_state(alias, state, None).unwrap();
    }

    /// A pty lane whose live watch proves it busy — the checkup must
    /// leave it alone.
    fn busy_pane(s: &Arc<Shared>, alias: &str) {
        let ctl = Arc::new(AgentCtl {
            adapter: Mutex::new(None),
            wake: Notify::new(),
            thread: Mutex::new(None),
            stall: Mutex::new(StallWatch {
                idle_samples: 0,
                idle_since: None,
                last_probe: Some(Probe {
                    idle: false,
                    reason: "busy marker".into(),
                    input_nonempty: false,
                    prompt_visible: false,
                    busy_marker: true,
                    approval_menu: false,
                    trust_prompt: false,
                    steerable: false,
                    queue_pending: false,
                }),
                ..StallWatch::default()
            }),
            cloud_held: std::sync::atomic::AtomicBool::new(false),
        });
        s.lifecycle
            .lock()
            .unwrap()
            .agents
            .insert(alias.to_string(), ctl);
        s.provider_env.set("CADENCE_DEVIN_ORG_ID", "org-test");
    }

    /// One tracker dir: project `tst` whose repo remote matches PR1,
    /// then the issues the test names.
    fn pm_scaffold(pm: &std::path::Path) {
        std::fs::create_dir_all(pm.join("notes")).unwrap();
        std::fs::write(
            pm.join("pm.yaml"),
            format!(
                "schema: 1\nnotes_dir: {}\nstatuses:\n- backlog\n- ready\n- doing\n- review\n- done\n- dropped\n",
                pm.join("notes").display()
            ),
        )
        .unwrap();
        std::fs::create_dir_all(pm.join("tst")).unwrap();
        std::fs::write(
            pm.join("tst/project.yaml"),
            "key: tst\nprefix: TST\nrepos:\n- remote: https://github.com/acme/app.git\n",
        )
        .unwrap();
    }

    /// An issue's `issue.md`: `status` then `extra` carries
    /// owner/paths/refs/blocked_by.
    fn issue(pm: &std::path::Path, id: &str, status: &str, extra: &str) {
        let dir = pm.join("tst").join(id);
        std::fs::create_dir_all(dir.join("reports")).unwrap();
        std::fs::write(
            dir.join("issue.md"),
            format!(
                "---\nid: {id}\ntitle: test {id}\nstatus: {status}\npriority: P2\n\
                 created: 2026-01-01T00:00:00Z\n{extra}---\n\nacceptance:\n- works\n"
            ),
        )
        .unwrap();
    }

    /// A `done` report filed by `agent` on `issue`; returns its file
    /// name, the `handled` key a record matches.
    fn done_report(
        pm: &std::path::Path,
        issue: &str,
        at: i64,
        agent: &str,
        sha: Option<&str>,
        pr: Option<&str>,
    ) -> String {
        let iso = crate::issue::time::iso(at);
        let compact: String = iso.chars().filter(|c| *c != '-' && *c != ':').collect();
        let name = format!("{compact}-{agent}.md");
        let mut front =
            format!("schema: cadence.report/2\nkind: done\ntask: {issue}\nagent: {agent}\n");
        if let Some(sha) = sha {
            front.push_str(&format!("sha: {sha}\n"));
        }
        if let Some(pr) = pr {
            front.push_str(&format!("pr: {pr}\n"));
        }
        std::fs::write(
            pm.join("tst").join(issue).join("reports").join(&name),
            format!(
                "---\n{front}---\n\n## Expected\n\nx\n\n## Evidence\n\nx\n\n\
                 ## Cause\n\nx\n\n## Correction\n\nx\n\n## Lesson\n\nx\n\n## Next\n\nx\n"
            ),
        )
        .unwrap();
        name
    }

    /// Plant a delivery record, mutated by `edit`.
    fn record(s: &Arc<Shared>, id: &str, edit: impl FnOnce(&mut Record)) {
        let mut all = delivery::load(&s.state_dir).unwrap();
        let mut r = Record::new(id, "tst", "w1", 1);
        edit(&mut r);
        all.insert(id.to_string(), r);
        delivery::save(&s.state_dir, &all).unwrap();
    }

    fn verdict(verdict: &str, sha: &str) -> VerdictRec {
        VerdictRec {
            verdict: verdict.to_string(),
            sha: sha.to_string(),
            reviewer: "r1".to_string(),
            summary: "needs changes".to_string(),
            report: "TST-1/reports/r.md".to_string(),
            at: 1,
        }
    }

    fn observed(head: &str, ci_green: bool) -> Observed {
        Observed {
            head: head.to_string(),
            pr_state: "OPEN".to_string(),
            ci_green,
            ..Observed::default()
        }
    }

    fn outcomes(s: &Shared, alias: &str) -> Vec<String> {
        s.store
            .events_tail(alias, 100)
            .unwrap()
            .iter()
            .filter(|e| e.kind == CHECKUP_EVENT)
            .map(|e| e.payload["outcome"].as_str().unwrap_or("?").to_string())
            .collect()
    }

    fn records(s: &Shared) -> BTreeMap<String, Record> {
        delivery::load(&s.state_dir).unwrap()
    }

    // ---- acceptance 1: a revise verdict or red CI gets one fix turn
    // on the same PR — never a second ticket ----

    #[test]
    fn revise_on_the_head_sends_one_fix_turn() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Working;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("revise", SHA1));
            r.revisions = 1;
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["fix"]);
        let fix = s.store.queued_head("w1").unwrap().expect("a fix turn");
        assert!(fix.id.starts_with("checkup-fix-tst-1-"), "{}", fix.id);
        assert!(fix.body.contains(PR1), "{}", fix.body);
        assert_eq!(fix.source, "review");
        // The fix is a turn, not a ticket: nothing dispatched.
        assert!(calls.lock().unwrap().is_empty());

        // While the fix turn still waits in the queue the lane is a
        // turn-holding lane, not an idle one.
        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["fix", "leave"]);
        // And still exactly one fix message exists.
        assert!(s.store.queued_head("w1").unwrap().is_some());
    }

    #[test]
    fn red_ci_on_the_head_sends_one_fix_turn() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Passed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("pass", SHA1));
            r.observed = Some(observed(SHA1, false));
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["fix"]);
        let fix = s.store.queued_head("w1").unwrap().unwrap();
        assert!(fix.body.contains("CI is failing"), "{}", fix.body);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_fix_turn_sent_and_unanswered_escalates_once() {
        let (_d, s, _calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Working;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("revise", SHA1));
            r.revisions = 1;
            r.handled = vec![name];
        });
        // The lane already got this head's fix turn — it took it and
        // went idle without a new done report.
        s.store
            .enqueue(
                "w1",
                "fix it",
                None,
                "checkup-fix-tst-1-111111111111",
                "review",
            )
            .unwrap();
        let Take::Message(m) = s.store.take_queued("w1").unwrap() else {
            panic!("the fix turn must be taken");
        };
        s.store.finish(&m, "completed", &json!({}), None).unwrap();

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["escalate"]);
        let esc = crate::master::escalations(&s.state_dir);
        assert!(esc.contains_key("TST-1/next-action"), "{esc:?}");
        // And the row is one Needs-you in the overview.
        let view = crate::overview::overview_cached(&s.state_dir, pm.path());
        let rows: Vec<_> = view["needs_me"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"].as_str() == Some("next_action"))
            .collect();
        assert_eq!(rows.len(), 1, "{:?}", view["needs_me"]);
        // A second pass records nothing new.
        s.checkup_tick();
        assert_eq!(crate::master::escalations(&s.state_dir).len(), 1);
    }

    // ---- acceptance 2: a green PR with no reviewer gets one free
    // reviewer — the loop's own pick, no author work in those files ----

    #[test]
    fn unstaffed_green_pr_routes_one_review_to_a_free_reviewer() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        staff(&s, "r1", "fake", "reviewer", "idle", None, None);
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        // The loop saw the done report but nobody was free to review.
        record(&s, "TST-1", |r| {
            r.state = State::Unstaffed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["review"]);
        let rec = &records(&s)["TST-1"];
        assert_eq!(rec.state, State::Reviewing);
        assert_eq!(rec.reviewer.as_deref(), Some("r1"));
        // The reviewer got the kickoff; the author got nothing.
        assert!(s.store.queued_head("r1").unwrap().is_some());
        assert!(s.store.queued_head("w1").unwrap().is_none());
        assert!(calls.lock().unwrap().is_empty());
    }

    /// CAD-591: each agent that the old "any idle peer" rule would pick
    /// sorts ahead of `rev`. The route lands on `rev` only when the
    /// implementer, the busy reviewer, the foreign-PM reviewer, and a
    /// `qa` team-role label are all refused.
    #[test]
    fn review_skips_implementers_busy_reviewers_and_foreign_pm_groups() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        staff(&s, "a-impl", "fake", "worker", "idle", None, None);
        staff(&s, "b-busy", "fake", "reviewer", "busy", None, None);
        staff(
            &s,
            "c-foreign",
            "fake",
            "reviewer",
            "idle",
            Some(r#"{"upstream":"other-pm"}"#),
            None,
        );
        staff(&s, "d-team", "fake", "worker", "idle", None, Some("qa"));
        staff(&s, "rev", "fake", "reviewer", "idle", None, None);
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Unstaffed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["review"]);
        assert_eq!(records(&s)["TST-1"].reviewer.as_deref(), Some("rev"));
        for alias in ["a-impl", "b-busy", "c-foreign", "d-team", "w1"] {
            assert!(
                s.store.queued_head(alias).unwrap().is_none(),
                "{alias} was handed the review"
            );
        }
        assert!(s.store.queued_head("rev").unwrap().is_some());
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn only_implementers_leave_the_review_unstaffed_in_needs_you() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        staff(&s, "rev", "fake", "worker", "idle", None, None);
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Unstaffed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["escalate"]);
        assert_eq!(records(&s)["TST-1"].state, State::Unstaffed);
        assert!(records(&s)["TST-1"].reviewer.is_none());
        assert!(s.store.queued_head("rev").unwrap().is_none());
        assert!(s.store.queued_head("w1").unwrap().is_none());
        assert!(calls.lock().unwrap().is_empty());
        let view = crate::overview::overview_cached(&s.state_dir, pm.path());
        let rows: Vec<_> = view["needs_me"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"].as_str() == Some("review_unstaffed"))
            .collect();
        assert_eq!(rows.len(), 1, "{:?}", view["needs_me"]);
    }

    /// CAD-591: a designated reviewer who is busy is not a fresh
    /// assignment. The kickoff is not queued, and Needs-you keeps the
    /// unstaffed row.
    #[test]
    fn busy_reviewer_pool_leaves_the_review_unstaffed_in_needs_you() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        staff(&s, "rev", "fake", "reviewer", "busy", None, None);
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Unstaffed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["escalate"]);
        assert_eq!(records(&s)["TST-1"].state, State::Unstaffed);
        assert!(records(&s)["TST-1"].reviewer.is_none());
        assert!(s.store.queued_head("rev").unwrap().is_none());
        assert!(s.store.queued_head("w1").unwrap().is_none());
        assert!(calls.lock().unwrap().is_empty());
        let view = crate::overview::overview_cached(&s.state_dir, pm.path());
        let rows: Vec<_> = view["needs_me"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"].as_str() == Some("review_unstaffed"))
            .collect();
        assert_eq!(rows.len(), 1, "{:?}", view["needs_me"]);
    }

    #[test]
    fn an_unrecorded_done_report_is_adopted_and_routed() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        staff(&s, "r1", "fake", "reviewer", "idle", None, None);
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "doing", "");
        done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["review"]);
        let rec = &records(&s)["TST-1"];
        assert_eq!(rec.state, State::Reviewing);
        assert_eq!(rec.reviewer.as_deref(), Some("r1"));
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_pr_with_nobody_to_review_is_needs_you_not_a_dispatch() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        // A ready ticket exists — but the lane's PR still needs its
        // review, so nothing else may start for it.
        issue(pm.path(), "TST-2", "ready", "paths:\n- src/elsewhere.rs\n");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Unstaffed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.handled = vec![name];
        });

        s.checkup_tick();
        // Nobody free: the record stays unstaffed — its row is the
        // operator's — and no ticket goes to the lane.
        assert_eq!(outcomes(&s, "w1"), ["escalate"]);
        assert_eq!(records(&s)["TST-1"].state, State::Unstaffed);
        assert!(calls.lock().unwrap().is_empty());
    }

    // ---- acceptance 3: a PR waiting on a person dispatches one safe
    // ready ticket; the waiting PR is untouched ----

    #[test]
    fn a_passed_pr_waiting_on_a_person_dispatches_one_safe_ticket() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        // The safe pick: ready, declared paths touching nothing live.
        issue(pm.path(), "TST-2", "ready", "paths:\n- src/other.rs\n");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Passed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("pass", SHA1));
            r.observed = Some(observed(SHA1, true));
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["dispatch"]);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[("TST-2".to_string(), "w1".to_string())]
        );
        // The waiting PR's record is untouched — nobody merged it.
        let rec = &records(&s)["TST-1"];
        assert_eq!(rec.state, State::Passed);
        // And the new work enters the loop.
        assert_eq!(records(&s)["TST-2"].worker, "w1");
    }

    #[test]
    fn a_candidate_sharing_files_with_an_open_pr_is_never_picked() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        // The waiting PR declares its surface via the issue's paths.
        issue(
            pm.path(),
            "TST-1",
            "review",
            "paths:\n- src/app.rs\nrefs:\n- kind: pr\n  url: https://github.com/acme/app/pull/7\n",
        );
        // Same file as the open PR — unsafe, and id-ordered first so
        // the pick proves the overlap skip, not a ranking accident.
        issue(pm.path(), "TST-2", "ready", "paths:\n- src/app.rs\n");
        // Disjoint — the safe pick.
        issue(pm.path(), "TST-3", "ready", "paths:\n- src/docs.rs\n");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Passed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("pass", SHA1));
            r.observed = Some(observed(SHA1, true));
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["dispatch"]);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[("TST-3".to_string(), "w1".to_string())]
        );
    }

    #[test]
    fn an_in_flight_lane_claims_its_files_too() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        // Another lane holds TST-9 in doing — its declared paths are
        // claimed even with nothing committed yet.
        issue(
            pm.path(),
            "TST-9",
            "doing",
            "paths:\n- src/live.rs\nrefs:\n- kind: worktree\n  path: /tmp/wt-tst9\n",
        );
        issue(pm.path(), "TST-2", "ready", "paths:\n- src/live.rs\n");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Passed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("pass", SHA1));
            r.observed = Some(observed(SHA1, true));
            r.handled = vec![name];
        });

        s.checkup_tick();
        // The only candidate overlapped a lane in flight — Needs-you.
        assert_eq!(outcomes(&s, "w1"), ["escalate"]);
        assert!(calls.lock().unwrap().is_empty());
        assert!(crate::master::escalations(&s.state_dir).contains_key("TST-1/next-action"));
    }

    #[test]
    fn a_ticket_done_but_awaiting_acceptance_claims_its_files() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        // Done on the tracker, PR still open — operator acceptance
        // pending; its paths stay claimed.
        issue(
            pm.path(),
            "TST-9",
            "done",
            "paths:\n- src/accept.rs\nrefs:\n- kind: pr\n  url: https://github.com/acme/app/pull/9\n",
        );
        issue(pm.path(), "TST-2", "ready", "paths:\n- src/accept.rs\n");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Passed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("pass", SHA1));
            r.observed = Some(observed(SHA1, true));
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["escalate"]);
        assert!(calls.lock().unwrap().is_empty());
    }

    // ---- acceptance 4: nothing safe → exactly one Needs-you row ----

    #[test]
    fn no_safe_candidate_records_one_needs_you_row() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Passed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("pass", SHA1));
            r.observed = Some(observed(SHA1, true));
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["escalate"]);
        assert!(calls.lock().unwrap().is_empty());
        let esc = crate::master::escalations(&s.state_dir);
        assert_eq!(esc.len(), 1, "{esc:?}");
        assert!(esc.contains_key("TST-1/next-action"));
        // The row is the operator's.
        let view = crate::overview::overview_cached(&s.state_dir, pm.path());
        let rows: Vec<_> = view["needs_me"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"].as_str() == Some("next_action"))
            .collect();
        assert_eq!(rows.len(), 1, "{:?}", view["needs_me"]);
        // Pass two: still one record, still one row.
        s.checkup_tick();
        assert_eq!(crate::master::escalations(&s.state_dir).len(), 1);
        assert_eq!(outcomes(&s, "w1"), ["escalate", "escalate"]);
    }

    // ---- acceptance 5: busy lanes are left alone; the action never
    // merges or touches a provider ----

    #[test]
    fn a_lane_with_a_turn_is_visited_but_never_actioned() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        issue(pm.path(), "TST-2", "ready", "paths:\n- src/o.rs\n");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Passed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("pass", SHA1));
            r.observed = Some(observed(SHA1, true));
            r.handled = vec![name];
        });
        // A queued turn is in flight — the lane is a turn-holding
        // lane, never an idle one.
        s.store
            .enqueue("w1", "work", None, "m-turn", "test")
            .unwrap();

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["leave"]);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_busy_pane_is_left_alone() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "pty");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        issue(pm.path(), "TST-2", "ready", "paths:\n- src/o.rs\n");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Passed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("pass", SHA1));
            r.observed = Some(observed(SHA1, true));
            r.handled = vec![name];
        });
        busy_pane(&s, "w1");

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["leave"]);
        assert!(calls.lock().unwrap().is_empty());
        assert!(crate::master::escalations(&s.state_dir).is_empty());
    }

    #[test]
    fn a_not_idle_agent_is_left_alone() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        s.store.set_agent_state("w1", "busy", None).unwrap();
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        issue(pm.path(), "TST-2", "ready", "paths:\n- src/o.rs\n");
        done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["leave"]);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn nothing_merges_and_no_provider_starts() {
        let (_d, s, _calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Passed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("pass", SHA1));
            r.observed = Some(observed(SHA1, true));
            r.handled = vec![name];
        });

        s.checkup_tick();
        // A mergeable-looking record stays Passed — the checkup is
        // not a merger.
        assert_eq!(records(&s)["TST-1"].state, State::Passed);
        let kinds: Vec<String> = s
            .store
            .events_tail(DAEMON_ALIAS, 100)
            .unwrap()
            .iter()
            .map(|e| e.kind.clone())
            .collect();
        assert!(
            kinds
                .iter()
                .all(|k| !k.contains("merge") && !k.contains("restart")),
            "{kinds:?}"
        );
    }

    // ---- one action, exactly ----

    #[test]
    fn one_action_per_idle_lane_per_pass() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        issue(pm.path(), "TST-2", "ready", "paths:\n- src/o.rs\n");
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        // Fix is due AND a safe ticket exists — exactly the fix goes
        // out; the lane is not asked to start a second ticket too.
        record(&s, "TST-1", |r| {
            r.state = State::Working;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("revise", SHA1));
            r.revisions = 1;
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["fix"]);
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(s.store.queued_head("w1").unwrap().unwrap().source, "review");
    }

    #[test]
    fn a_raced_ticket_leaves_ready_between_pick_and_dispatch() {
        // dispatch_one itself re-checks under dispatch_lock: called
        // directly on a ticket already doing, it refuses.
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-2", "doing", "paths:\n- src/o.rs\n");
        let p = Pm::at(pm.path()).unwrap();
        let agent = s.store.agent("w1").unwrap();
        let (outcome, _) = s.dispatch_one(&p, &agent, "TST-2", "tst").unwrap();
        assert_eq!(outcome, Outcome::Leave);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_ticket_owned_by_someone_else_is_never_picked() {
        let (_d, s, calls) = shared();
        worker(&s, "w1", "fake");
        let pm = tempfile::TempDir::new().unwrap();
        pm_scaffold(pm.path());
        s.provider_env
            .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
        issue(pm.path(), "TST-1", "review", "");
        // Ready and safe — but bound to another lane.
        issue(
            pm.path(),
            "TST-2",
            "ready",
            "owner: w9\npaths:\n- src/o.rs\n",
        );
        let name = done_report(
            pm.path(),
            "TST-1",
            1_700_000_000,
            "w1",
            Some(SHA1),
            Some(PR1),
        );
        record(&s, "TST-1", |r| {
            r.state = State::Passed;
            r.head = Some(SHA1.to_string());
            r.pr = Some(PR1.to_string());
            r.verdict = Some(verdict("pass", SHA1));
            r.observed = Some(observed(SHA1, true));
            r.handled = vec![name];
        });

        s.checkup_tick();
        assert_eq!(outcomes(&s, "w1"), ["escalate"]);
        assert!(calls.lock().unwrap().is_empty());
    }
}
