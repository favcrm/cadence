//! Overview row assembly from collected source results.
//! Contextual tracker/runtime/git reads retain their original assembly order.

use super::github::compute_drift;
use super::sources::Snapshot;
use super::{
    agent_items, agent_project, build_repo_match, checks_green, classify_needs,
    cmd_issue_set_ready, cmd_issue_show, degraded, delivery_items, display_text, git_text,
    group_members, item, main_ci_view, master_login_item, merge_by_subject, parse_iso,
    platform_effect_items, relay_attention_items, rollup_span, scope_rows, sort_needs,
    verdict_state, Item, Options, Owners, StatusClock, CMD_ISSUE_SYNC, CMD_UPGRADE_LATEST_MAIN,
    ESCALATE_AFTER_SECS, SHORT_TITLE_MAX, STATUS_CLOCK_BUDGET,
};
use crate::inbox;
use crate::issue::line_times::LineTimes;
use crate::issue::{self, board, claim, report};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The ticket hold and the lane fence can describe the same owned unknown
/// turn. Keep the fence's recovery control and both causes, not two prompts.
fn coalesce_unattended_fences(needs: &mut [Item], state_dir: &Path) -> Result<(), String> {
    if !needs
        .iter()
        .any(|row| row.json["unattended_turn"].is_string())
    {
        return Ok(());
    }
    use rusqlite::OptionalExtension;
    let conn = crate::store::open_read_only(&state_dir.join("cadence.sqlite3"))
        .map_err(|error| error.to_string())?;
    for index in 0..needs.len() {
        if needs[index].json["kind"] != "unattended_held" {
            continue;
        }
        let (Some(turn), Some(worker)) = (
            needs[index].json["unattended_turn"].as_str(),
            needs[index].agents.first(),
        ) else {
            continue;
        };
        let unknown = conn
            .query_row(
                "SELECT 1 FROM messages WHERE id=?1 AND alias=?2 AND state IN ('unknown','failed') \
                 AND ((source='dispatch' AND issue=?3 AND worktree IS NOT NULL) \
                      OR (source='review' AND sender_uid IS NULL AND (issue IS NULL OR issue=?3)))",
                rusqlite::params![turn, worker, needs[index].subject.1],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if unknown != Some(1) {
            continue;
        }
        let Some(fence) = needs.iter().position(|row| {
            row.json["kind"] == "fenced" && row.subject.0 == "agent" && row.subject.1 == *worker
        }) else {
            continue;
        };
        let (held, fenced) = if index < fence {
            let (before, after) = needs.split_at_mut(fence);
            (&mut before[index], &mut after[0])
        } else {
            let (before, after) = needs.split_at_mut(index);
            (&mut after[0], &mut before[fence])
        };
        // The owned turn identifies the affected project. The lane's
        // registration cwd can still name its previous project.
        fenced.json["project"] = held.json["project"].clone();
        held.subject = fenced.subject.clone();
        held.json["subject"] = fenced.json["subject"].clone();
        held.rank = fenced.rank.saturating_add(1);
    }
    Ok(())
}

pub(super) fn assemble(
    state_dir: &Path,
    opts: &Options,
    snapshot: Snapshot<'_>,
) -> Result<Value, String> {
    let Snapshot {
        now,
        pm,
        projects,
        repo_paths,
        slug_project,
        slug_clone,
        has_github,
        sources,
        views,
        gh_repos,
        gh_state,
    } = snapshot;
    let daemon = &sources.daemon;
    let monitoring_view = sources.monitoring.clone();
    let views = views.as_deref();
    let mut degraded_notes = daemon.degraded.clone();
    if has_github {
        if let Some(e) = gh_state["error"].as_str() {
            degraded_notes.push(degraded("github", "", e));
        }
    }
    let members = match opts.scope.group.as_deref() {
        Some(root) => Some(group_members(daemon, root)?),
        None => None,
    };

    // ---- daemon rows ----
    let mut needs: Vec<Item> = Vec::new();
    let mut panes_idle = daemon.reachable;
    // An old daemon without `agent_probe` cannot confirm a pane is idle
    // — the drift row must say so instead of vanishing quietly.
    let mut probes_unknown = false;
    for (a, probe) in daemon.agents.iter().zip(&daemon.probes) {
        let project = agent_project(a, &repo_paths);
        needs.extend(agent_items(a, probe, &project, now));
        needs.extend(master_login_item(a, state_dir, &project, now));
        panes_idle &= !probe.holds_drift;
        probes_unknown |= probe.probe_unknown;
    }
    if daemon.reachable {
        needs.extend(platform_effect_items(state_dir, now, opts.probe_timeout));
    }
    needs.extend(relay_attention_items(state_dir, now));

    // ---- tracker rows ----
    // Lowercased headRefName of every open PR — an issue in review
    // counts as "has a PR" when a `cadence/<id-lowercase>-…` branch is
    // open, even without an explicit `pr` ref.
    let mut open_pr_branches: Vec<String> = Vec::new();
    for data in gh_repos.values() {
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            if let Some(head) = pr["headRefName"].as_str() {
                open_pr_branches.push(head.to_lowercase());
            }
        }
    }
    // `cadence/<id-lowercase>-` branch prefix → (issue id, owner): a
    // PR row belongs to the issue's owner for `--group`.
    let mut branch_issue: Vec<(String, String, Option<String>)> = Vec::new();
    let mut projects_out = Vec::new();
    if let (Some(pm), Some(views)) = (&pm, views) {
        let mut status_of: HashMap<String, String> = HashMap::new();
        for v in views {
            status_of.insert(v.issue.front.id.clone(), v.status.clone());
            branch_issue.push((
                crate::worktree::layout::issue_branch_prefix(&v.issue.front.id),
                v.issue.front.id.clone(),
                v.issue.front.owner.clone(),
            ));
        }
        let view_of: HashMap<&str, &board::View> = views
            .iter()
            .map(|v| (v.issue.front.id.as_str(), v))
            .collect();
        let line_times = LineTimes::load(&pm.dir, STATUS_CLOCK_BUDGET).ok();
        let mut clock = StatusClock::new(line_times.as_ref());
        let mut intake: Vec<Item> = Vec::new();
        let mut backlog_stale: Vec<Item> = Vec::new();
        let escalations = crate::master::escalations(state_dir);
        // CAD-484: a next-action row hides only when its lane is
        // provably not at rest — the daemon answered and the lane is
        // busy, queued or working. An unreachable daemon or an
        // unlisted alias cannot disprove the flag, so the row stands.
        let lane_working: HashMap<String, bool> = daemon
            .agents
            .iter()
            .zip(&daemon.probes)
            .filter_map(|(a, p)| {
                let alias = a["alias"].as_str()?;
                let queued = a["inbox"]["queued"]
                    .as_i64()
                    .or_else(|| p.show.as_ref().and_then(|s| s["queued"].as_i64()))
                    .unwrap_or(0);
                Some((
                    alias.to_string(),
                    a["state"].as_str() != Some("idle") || p.holds_drift || queued > 0,
                ))
            })
            .collect();
        for v in views {
            let id = v.issue.front.id.as_str();
            let project = v.issue.project.as_str();
            let owner = v.issue.front.owner.as_deref().unwrap_or_default();
            let age = parse_iso(&v.issue.front.created)
                .map(|c| now - c)
                .unwrap_or(0);
            let branch_prefix = crate::worktree::layout::issue_branch_prefix(id);
            let open_pr = v
                .issue
                .front
                .refs
                .iter()
                .any(|r| r.kind == "pr" && r.closed != Some(true))
                || open_pr_branches
                    .iter()
                    .any(|b| b.starts_with(&branch_prefix));
            let gate_tag = v
                .issue
                .front
                .tags
                .iter()
                .any(|t| matches!(t.as_str(), "plan-ready" | "parked" | "idea-stale"));
            if v.status == "review" && !open_pr && !gate_tag {
                needs.push(
                    item(
                        70,
                        "review_no_pr",
                        &format!("{id} in review with no open PR"),
                        age,
                        project,
                        None,
                        &cmd_issue_show(id),
                    )
                    .about("issue", id)
                    .for_agent(owner)
                    .owned_by(Some(owner))
                    .since(clock.since(v)),
                );
            }
            let unblocked = !v.issue.front.blocked_by.is_empty()
                && v.issue
                    .front
                    .blocked_by
                    .iter()
                    .all(|b| status_of.get(b).map(String::as_str) == Some("done"));
            if !matches!(v.status.as_str(), "done" | "dropped") && unblocked {
                // Unblocked when the last blocker reached done; one
                // blocker without a clock leaves the row without one.
                let since = v
                    .issue
                    .front
                    .blocked_by
                    .iter()
                    .map(|b| view_of.get(b.as_str()).and_then(|bv| clock.since(bv)))
                    .collect::<Option<Vec<i64>>>()
                    .and_then(|ts| ts.into_iter().max());
                needs.push(
                    item(
                        80,
                        "blocked_ready",
                        &format!("{id} unblocked — blockers all done"),
                        age,
                        project,
                        None,
                        &cmd_issue_set_ready(id),
                    )
                    .about("issue", id)
                    .for_agent(owner)
                    .owned_by(Some(owner))
                    .since(since),
                );
            }
            // CAD-339 Needs-you: a plan waiting for the operator's
            // decision, and every open question the master escalated —
            // with the master's summary, the question and its options.
            if let Some(plan) = v
                .issue
                .front
                .plan
                .as_ref()
                .filter(|p| p.state == "proposed")
            {
                let since = parse_iso(&plan.proposed_at);
                let mut row = item(
                    25,
                    "plan",
                    &format!(
                        "{id} plan proposed by {} — {} ({} tickets)",
                        plan.proposed_by,
                        v.issue.front.title,
                        plan.tickets.len()
                    ),
                    since.map_or(age, |t| now - t),
                    project,
                    None,
                    &format!("cadence plan show {id} && cadence plan approve {id}"),
                )
                .about("issue", id)
                .since(since)
                .with_display(Some(&v.issue.front.title), None);
                row.json["plan"] = json!({"epic": id, "proposed_by": plan.proposed_by,
                                          "tickets": plan.tickets});
                needs.push(row);
            }
            // Only the daemon's escalation record puts a question here —
            // a report file never can (review round 1, I3); reports are
            // parsed only for tickets that have one.
            let escalated_here = escalations.keys().any(|k| k.starts_with(&format!("{id}/")));
            let open = if escalated_here {
                issue::task_report::open_questions(&v.issue.dir, id)
            } else {
                vec![]
            };
            for q in open {
                let key = format!("{id}/{}", q["name"].as_str().unwrap_or_default());
                let Some(up) = escalations.get(&key).and_then(Value::as_object) else {
                    continue;
                };
                let since = q["at"].as_str().and_then(parse_iso);
                let mut row = item(
                    20,
                    "question",
                    &format!(
                        "{id} question from {} — {}",
                        q["agent"].as_str().unwrap_or_default(),
                        q["impact"].as_str().unwrap_or_default()
                    ),
                    since.map_or(age, |t| now - t),
                    project,
                    None,
                    &format!(
                        "cadence issue show {id}  # answer: cadence report file --task {id} \
                         --kind answer (answers: {})",
                        q["name"].as_str().unwrap_or_default()
                    ),
                )
                .about(
                    "report",
                    &format!("{id}/{}", q["name"].as_str().unwrap_or_default()),
                )
                .for_agent(q["agent"].as_str().unwrap_or_default())
                .since(since)
                .with_display(
                    // A refused summary falls back to the body.
                    up.get("summary")
                        .and_then(Value::as_str)
                        .filter(|s| display_text(s, SHORT_TITLE_MAX).is_some())
                        .or(q["body"].as_str()),
                    q["impact"].as_str(),
                );
                row.json["question"] = json!({
                    "issue": id, "report": q["name"], "agent": q["agent"],
                    "options": q["options"], "impact": q["impact"], "body": q["body"],
                });
                row.json["summary"] = up.get("summary").cloned().unwrap_or(Value::Null);
                row.json["escalated_by"] = up.get("by").cloned().unwrap_or(Value::Null);
                needs.push(row);
            }
            // CAD-477: a blocked report the checkup escalated is the
            // operator's to unblock — one row while it is still the
            // issue's open state; a newer `done` report or a done
            // issue clears it.
            if escalated_here {
                let reports = issue::task_report::list(&v.issue.dir, id);
                for b in reports
                    .iter()
                    .filter(|r| r["kind"].as_str() == Some("blocked"))
                {
                    let name = b["name"].as_str().unwrap_or_default();
                    let Some(up) = escalations
                        .get(&format!("{id}/{name}"))
                        .and_then(Value::as_object)
                    else {
                        continue;
                    };
                    if !issue::task_report::blocked_open(&reports, b, &v.status) {
                        continue;
                    }
                    let since = b["at"].as_str().and_then(parse_iso);
                    let mut row = item(
                        20,
                        "blocked",
                        &format!(
                            "{id} blocked — {} reports it cannot proceed",
                            b["agent"].as_str().unwrap_or_default()
                        ),
                        since.map_or(age, |t| now - t),
                        project,
                        None,
                        &format!("cadence issue show {id}"),
                    )
                    .about("report", &format!("{id}/{name}"))
                    .for_agent(b["agent"].as_str().unwrap_or_default())
                    .since(since);
                    row.json["blocked"] = json!({
                        "issue": id, "report": name, "agent": b["agent"], "body": b["body"],
                    });
                    row.json["summary"] = up.get("summary").cloned().unwrap_or(Value::Null);
                    row.json["escalated_by"] = up.get("by").cloned().unwrap_or(Value::Null);
                    needs.push(row);
                }
            }
            // CAD-484: the checkup's one Needs-you when an idle lane
            // had no safe next step. The record is keyed
            // `{issue}/next-action`; the row hides the moment the lane
            // provably works again — a queued kickoff, a running turn,
            // a busy pane — and never needs a record delete.
            if let Some(up) = escalations
                .get(&format!("{id}/next-action"))
                .and_then(Value::as_object)
                .filter(|u| u["kind"].as_str() == Some("next_action"))
            {
                let agent = up["agent"].as_str().unwrap_or_default();
                if !lane_working.get(agent).copied().unwrap_or(false) {
                    let since = up["at"].as_str().and_then(parse_iso);
                    let mut row = item(
                        20,
                        "next_action",
                        &format!(
                            "{id} — {} is idle with no safe next step — {}",
                            agent,
                            up["summary"].as_str().unwrap_or_default()
                        ),
                        since.map_or(age, |t| now - t),
                        project,
                        None,
                        &format!("cadence issue show {id}"),
                    )
                    .about("issue", id)
                    .for_agent(agent)
                    .since(since);
                    row.json["next_action"] = json!({"issue": id, "agent": agent});
                    row.json["summary"] = up.get("summary").cloned().unwrap_or(Value::Null);
                    row.json["escalated_by"] = up.get("by").cloned().unwrap_or(Value::Null);
                    needs.push(row);
                }
            }
            // CAD-139: the idea pipeline stops here. The operator
            // approves, rejects, or parks; nothing else is dispatched.
            if v.status == "review" && v.issue.front.tags.iter().any(|t| t == "plan-ready") {
                needs.push(
                    item(
                        20,
                        "idea_plan",
                        &format!(
                            "idea plan ready for your decision — {id} {}",
                            v.issue.front.title
                        ),
                        age,
                        project,
                        None,
                        &format!("cadence idea decide {id} approve"),
                    )
                    .about("issue", id)
                    .since(clock.since(v)),
                );
            }
            if v.status == "backlog"
                && v.issue.front.tags.iter().any(|t| t == "idea")
                && v.issue.front.duplicate_of.is_some()
            {
                let other = v.issue.front.duplicate_of.as_deref().unwrap_or("");
                needs.push(
                    item(
                        20,
                        "idea_duplicate",
                        &format!("{id} looks like {other} — decide whether to keep it"),
                        age,
                        project,
                        None,
                        &format!("cadence issue show {id}"),
                    )
                    .about("issue", id)
                    .since(clock.since(v)),
                );
            }
            // CAD-812: a `backlog`/`ready` leaf the groom pass flagged
            // `needs-triage` — the requirement may have drifted. Surfaces
            // under `blocked_ready` (a flag, not a pick gate): oldest
            // first, for the owner to re-confirm or re-scope. Collected
            // and capped like intake so a pile of stale tickets cannot
            // bury real work.
            if matches!(v.status.as_str(), "backlog" | "ready")
                && v.issue.front.tags.iter().any(|t| t == "needs-triage")
            {
                let since = v.issue.front.last_groomed_at.as_deref().and_then(parse_iso);
                backlog_stale.push(
                    item(
                        82,
                        "backlog_stale",
                        &format!("{id} needs-triage — groom flagged drift"),
                        age,
                        project,
                        None,
                        &cmd_issue_show(id),
                    )
                    .about("issue", id)
                    .for_agent(owner)
                    .owned_by(Some(owner))
                    .since(since),
                );
            }
            // `cadence report` intake: a backlog-tagged row surfaces
            // until triage moves it off backlog — the effective status
            // (notes-derived counts too) is what clears it.
            if v.status == "backlog" && v.issue.front.tags.iter().any(|t| t == "intake") {
                let kind_tag = v
                    .issue
                    .front
                    .kind
                    .as_deref()
                    .or_else(|| {
                        v.issue
                            .front
                            .tags
                            .iter()
                            .find(|t| *t != "intake")
                            .map(String::as_str)
                    })
                    .unwrap_or("intake");
                intake.push(
                    item(
                        85,
                        "intake",
                        &format!("{id} {kind_tag} report — {}", v.issue.front.title),
                        age,
                        project,
                        None,
                        &format!("cadence report show {id}"),
                    )
                    .about("issue", id)
                    .for_agent(owner)
                    .owned_by(Some(owner)),
                );
            }
        }
        // Cap the intake block — hundreds of untriaged reports must not
        // bury real work. Oldest first, then one summary row.
        let intake_extra = cap_oldest(&mut intake);
        // Clocks only for the rows that surface — one read each.
        for it in &mut intake {
            let since = view_of
                .get(it.subject.1.as_str())
                .and_then(|v| clock.since(v));
            it.set_since(since);
        }
        if let Some(extra) = intake_extra {
            intake.push(
                item(
                    85,
                    "intake",
                    &format!("… {extra} more intake reports"),
                    0,
                    "",
                    None,
                    "cadence report ls",
                )
                .about("report", "intake-overflow"),
            );
        }
        // Cap the stale-backlog block the same way — oldest first, one
        // summary row past the cap.
        let stale_extra = cap_oldest(&mut backlog_stale);
        if let Some(extra) = stale_extra {
            backlog_stale.push(
                item(
                    82,
                    "backlog_stale",
                    &format!("… {extra} more stale backlog items"),
                    0,
                    "",
                    None,
                    "cadence issue ls --project <p>",
                )
                .about("issue", "backlog-stale-overflow"),
            );
        }
        needs.extend(backlog_stale);
        needs.extend(intake);
        let mut delivery = delivery_items(state_dir, now);
        for row in delivery
            .iter_mut()
            .filter(|r| r.json["kind"] == "merge_decision")
        {
            let title = row.json["merge"]["issue"]
                .as_str()
                .and_then(|i| view_of.get(i))
                .map(|bv| bv.issue.front.title.clone());
            row.set_display(title.as_deref(), None);
        }
        needs.extend(delivery);
        if !clock.skipped.is_empty() {
            degraded_notes.push(degraded(
                "tracker_status_time",
                "",
                format!(
                    "{} issue(s) past the status-time budget — those rows escalate by owner only",
                    clock.skipped.len()
                ),
            ));
        }
        // CAD-383: in-flight claims per project, with their age.
        let claim_clock = claim::Clock::new(line_times.as_ref());
        // CAD-378: the review loop's recorded PR per issue, for lanes
        // whose tracker carries no `pr` ref.
        let delivery_prs: std::collections::BTreeMap<String, String> =
            crate::delivery::records(state_dir)
                .into_values()
                .filter(|r| !r.state.terminal())
                .filter_map(|r| r.pr.map(|pr| (r.issue, pr)))
                .collect();
        for p in &projects {
            if opts.scope.project.as_deref().is_some_and(|k| k != p.key) {
                continue;
            }
            let mut open_by_status = serde_json::Map::new();
            let mut oldest_review: Option<i64> = None;
            let mut claims: Vec<Value> = Vec::new();
            for v in views.iter().filter(|v| v.issue.project == p.key) {
                if matches!(v.status.as_str(), "done" | "dropped") {
                    continue;
                }
                let front = &v.issue.front;
                if matches!(v.status.as_str(), "doing" | "review")
                    && !claim::holders(front).is_empty()
                {
                    let since = claim_clock.since(&p.key, front);
                    let mut row = claim::row(&p.key, front, since, now);
                    row["status"] = json!(v.status);
                    claims.push(row);
                }
                let n = open_by_status
                    .get(&v.status)
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                open_by_status.insert(v.status.clone(), json!(n + 1));
                if v.status == "review" {
                    let age = parse_iso(&v.issue.front.created)
                        .map(|c| now - c)
                        .unwrap_or(0);
                    oldest_review = Some(oldest_review.map_or(age, |o| o.max(age)));
                }
            }
            // CAD-378: open lanes, the code areas they plan or change,
            // and overlaps (the board's overlay); a lane with a PR that
            // changes an area owned by someone else is a Needs-you row
            // until the owner's PM or the operator acks it.
            let (areas, areas_error) = issue::areas::load_or_error(&pm.dir, &p.key);
            let project_issues: Vec<&issue::board::Issue> = views
                .iter()
                .filter(|v| v.issue.project == p.key)
                .map(|v| &v.issue)
                .collect();
            let lanes = issue::areas::open_lanes(state_dir, &project_issues, &delivery_prs);
            let acks = issue::areas::acks(state_dir);
            let has_pr = |l: &issue::areas::Lane| {
                let prefix = crate::worktree::layout::issue_branch_prefix(&l.issue);
                open_pr_branches.iter().any(|b| b.starts_with(&prefix))
            };
            for need in issue::areas::ack_needs(&areas, &lanes, &acks, has_pr) {
                let owner = need.area.pm.clone();
                let mut row = item(
                    28,
                    "area_ack",
                    &need.title(),
                    0,
                    &need.project,
                    need.pr.as_deref(),
                    &issue::areas::cmd_ack(&need.issue, &need.area.name),
                )
                .about("issue", &need.issue)
                .for_agent(owner.as_deref().unwrap_or_default())
                .owned_by(Some(owner.as_deref().unwrap_or(inbox::OPERATOR)));
                row.json["area"] = json!({
                    "issue": need.issue, "area": need.area.name, "owner": need.area.owner(),
                    "pm": need.area.pm, "files": need.files, "pr": need.pr,
                    "worker": need.worker,
                });
                needs.push(row);
            }
            projects_out.push(json!({
                "key": p.key,
                "open_by_status": open_by_status,
                "oldest_review_age": oldest_review,
                "claims": claims,
                "lanes": issue::areas::overlay(&areas, &lanes),
                "areas": areas.iter().map(|a| a.name.clone()).collect::<Vec<_>>(),
                "areas_error": areas_error,
            }));
        }
        // Tracker behind its upstream — local refs only, never a fetch.
        if let Ok(behind) = git_text(
            &pm.dir,
            &[
                "rev-list".into(),
                "--count".into(),
                "HEAD..@{upstream}".into(),
            ],
        ) {
            if let Ok(n) = behind.trim().parse::<i64>() {
                if n > 0 {
                    needs.push(
                        item(
                            110,
                            "tracker_behind",
                            &format!("tracker {n} commit(s) behind upstream"),
                            0,
                            "",
                            None,
                            CMD_ISSUE_SYNC,
                        )
                        .about("tracker", "pm"),
                    );
                }
            }
        }
    }

    // ---- GitHub rows: merge-ready, verdict-less, main CI ----
    let mut main_ci: Vec<Value> = Vec::new();
    for (slug, data) in &gh_repos {
        let project = slug_project.get(slug).cloned().unwrap_or_default();
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            let n = pr["number"].as_i64().unwrap_or(0);
            let title = pr["title"].as_str().unwrap_or("");
            let url = pr["url"].as_str().map(str::to_string);
            let rollup = pr["statusCheckRollup"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let age = pr["updatedAt"]
                .as_str()
                .and_then(parse_iso)
                .map(|u| now - u)
                .unwrap_or(0);
            let head_ref = pr["headRefName"].as_str().unwrap_or("").to_lowercase();
            let owner = branch_issue
                .iter()
                .find(|(prefix, _, _)| head_ref.starts_with(prefix.as_str()))
                .and_then(|(_, _, owner)| owner.clone())
                .unwrap_or_default();
            let subject = format!("{slug}#{n}");
            // Rollup times for this head (CAD-253): a CheckRun's
            // `completedAt`, a status context's `startedAt`.
            let (first_check, last_check) = rollup_span(&rollup);
            match verdict_state(&rollup).as_deref() {
                Some("SUCCESS") if checks_green(&rollup) => {
                    // The verdict binds this head — a push after it
                    // makes the copied command refuse instead of
                    // merging an unreviewed head.
                    let head = pr["headRefOid"].as_str().unwrap_or("");
                    needs.push(
                        item(
                            10,
                            "merge",
                            &format!("PR #{n} {title} — verdict pass, checks green"),
                            age,
                            &project,
                            url.as_deref(),
                            &format!(
                                "gh pr merge {n} --repo {slug} --squash --admin --match-head-commit {head}"
                            ),
                        )
                        .about("pr", &subject)
                        .for_agent(&owner)
                        .owned_by(Some(&owner))
                        // Merge-ready once the last check or verdict landed.
                        .since(last_check),
                    );
                }
                Some("SUCCESS") | Some("FAILURE") | Some("ERROR") => {}
                _ => needs.push(
                    item(
                        60,
                        "pr_no_verdict",
                        &format!("PR #{n} {title} — no verdict"),
                        age,
                        &project,
                        url.as_deref(),
                        &format!("gh pr view {n} --repo {slug}"),
                    )
                    .about("pr", &subject)
                    .for_agent(&owner)
                    .owned_by(Some(&owner))
                    // Verdict-less since this head's first check started;
                    // a head with no checks yet has no clock.
                    .since(first_check),
                ),
            }
        }
        let clone = slug_clone.get(slug).map(PathBuf::as_path);
        let (view, rows) = main_ci_view(slug, &project, data, clone, now);
        if let Some(e) = view["error"].as_str() {
            degraded_notes.push(degraded("github_ci", slug, e));
        }
        if !view.is_null() && opts.scope.project.as_deref().is_none_or(|k| k == project) {
            main_ci.push(view);
        }
        needs.extend(rows);
    }
    main_ci.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    // ---- deploy drift: is what we merged actually running ----
    let info = daemon.info.clone();
    let mut drift = if !daemon.reachable {
        json!({"matched": false, "reason": "daemon unreachable — cannot tell"})
    } else if info.is_none() {
        // Reachable but predates `daemon_info` — the build commit is
        // unreadable, so drift is unknowable, never zero.
        json!({
            "matched": false,
            "reason": "daemon build unknown (daemon predates daemon_info) — restart to enable drift",
        })
    } else {
        match build_repo_match(&projects) {
            None => json!({
                "matched": false,
                "build_commit": info.as_ref().map(|i| i["build_commit"].clone()).unwrap_or(json!("unknown")),
                "reason": "no tracker repo matches the build — cannot tell",
            }),
            Some((key, repo)) => {
                let build = info
                    .as_ref()
                    .and_then(|i| i["build_commit"].as_str())
                    .unwrap_or("unknown");
                let mut d = compute_drift(&repo, build);
                d["matched"] = json!(true);
                d["project"] = json!(key);
                d
            }
        }
    };
    if drift["known"].as_bool().unwrap_or(false) && drift["count"].as_i64().unwrap_or(0) > 0 {
        if panes_idle {
            let n = drift["count"].as_i64().unwrap_or(0);
            let project = drift["project"].as_str().unwrap_or("");
            needs.push(
                item(
                    50,
                    "drift",
                    &format!("{n} merged commit(s) not running — all panes idle"),
                    0,
                    project,
                    None,
                    CMD_UPGRADE_LATEST_MAIN,
                )
                .about("deploy", project),
            );
        } else {
            // Held back, but say why — a busy pane is different from
            // a daemon that cannot answer `agent_probe` at all.
            drift["held"] = json!(if probes_unknown {
                "cannot tell whether panes are idle — daemon predates agent_probe"
            } else {
                "a pane is busy — restart only when idle"
            });
        }
    }

    // CAD-615: a pending permission request is the operator's to decide.
    for req in crate::master_perm::board_requests(state_dir, now).unwrap_or_default() {
        let age = (now - req.created).max(0);
        let command = req.argv.join(" ");
        let title = if req.decision_label.is_empty() {
            format!(
                "master wants to run: {command} — risk {}",
                match req.risk {
                    crate::master_perm::Risk::Low => "low",
                    crate::master_perm::Risk::Medium => "medium",
                    crate::master_perm::Risk::High => "high",
                }
            )
        } else {
            format!("{} — {command}", req.decision_label)
        };
        let mut row = item(
            if req.status == "pending" { 15 } else { 40 },
            "master_permission",
            &title,
            age,
            "",
            None,
            &format!("cadence master allow-once {}", req.id),
        )
        .about("permission", &req.id)
        .since(Some(req.created))
        .with_display(
            // The title is the label, else the sanitized reason (omitted
            // when refused); the full reason is shown separately as data.
            Some(req.decision_label.as_str())
                .filter(|l| !l.is_empty())
                .or(Some(req.reason.as_str())),
            (!req.decision_label.is_empty()).then_some(req.reason.as_str()),
        );
        row.json["permission"] = crate::master_perm::request_json(&req);
        row.json["reason"] = json!(req.reason);
        needs.push(row);
    }

    if let Err(error) = coalesce_unattended_fences(&mut needs, state_dir) {
        degraded_notes.push(degraded("unattended_escalation", "", error));
    }

    classify_needs(
        &mut needs,
        &Owners::new(daemon.reachable, &daemon.agents),
        now,
        ESCALATE_AFTER_SECS,
    );
    let mut needs = scope_rows(
        merge_by_subject(needs),
        opts.scope.project.as_deref(),
        members.as_deref(),
    );
    sort_needs(&mut needs);
    // CAD-574: the operator's dismissals suppress their subjects for
    // every reader of this build — a snooze's clock is judged here.
    let dismissed = crate::needs_dismiss::dismissed(state_dir);
    if !dismissed.is_empty() {
        needs.retain(|i| {
            !crate::needs_dismiss::row_suppressed(
                &json!({"subject": {"kind": i.subject.0, "id": i.subject.1.as_str()},
                        "since": i.since, "age": i.age}),
                &dismissed,
                now,
            )
        });
    }
    let daemon_json = match info {
        Some(mut i) => {
            i["reachable"] = json!(daemon.reachable);
            i
        }
        None if daemon.reachable => json!({
            "reachable": true,
            "info": "daemon predates daemon_info — build identity unreadable",
        }),
        None => json!({"reachable": false}),
    };
    Ok(json!({
        "needs_me": needs.iter().map(|i| i.json.clone()).collect::<Vec<_>>(),
        "drift": drift,
        "projects": projects_out,
        "github": gh_state,
        "main_ci": main_ci,
        "daemon": daemon_json,
        "monitoring": monitoring_view,
        "degraded": degraded_notes,
        "scope": {"project": opts.scope.project, "group": opts.scope.group},
        "generated_at": now,
    }))
}

/// Keep the oldest rows in stable order and return the overflow count.
fn cap_oldest(rows: &mut Vec<Item>) -> Option<usize> {
    rows.sort_by_key(|i| std::cmp::Reverse(i.age));
    if rows.len() <= report::NEEDS_ME_CAP {
        return None;
    }
    let extra = rows.len() - report::NEEDS_ME_CAP;
    rows.truncate(report::NEEDS_ME_CAP);
    Some(extra)
}
