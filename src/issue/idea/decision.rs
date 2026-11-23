//! Idea pipeline operator decision recording and child-ticket creation.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::{set_status_tags_if_rev, Decision, Proposed, Record};
use crate::error::{Error, Result};
use crate::issue::{board, time, write, Pm};

fn valid_park_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let (y, m, d) = (&date[0..4], &date[5..7], &date[8..10]);
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<u16>(), m.parse::<u8>(), d.parse::<u8>()) else {
        return false;
    };
    y >= 1970 && (1..=12).contains(&m) && (1..=31).contains(&d)
}

/// Record the operator's decision and, on approve, create exactly the
/// proposed backlog children. A second approve or reject returns the
/// existing object. A park can still be approved or rejected later.
///
/// CAD-140: `expect_rev` binds the decision to the issue shown and
/// `actor` names who decided (the daemon caller threads the board's
/// `request_actor`). The status/tags write carries the revision — the
/// compare half of compare-and-swap, inside the tracker lock — and
/// runs BEFORE any child is created, so a stale decision refuses
/// with nothing written.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    pm: &Pm,
    records: &mut BTreeMap<String, Record>,
    issue: &str,
    action: &str,
    reason: Option<&str>,
    park_until: Option<&str>,
    expect_rev: &str,
    actor: &str,
) -> Result<Value> {
    let (project, proposed, prior) = {
        let rec = records.get(issue).ok_or_else(|| {
            Error::rejected(format!(
                "{issue} is not in the idea pipeline — it has no research record"
            ))
        })?;
        if matches!(rec.state.as_str(), "approved" | "rejected") {
            return Err(Error::rejected(format!(
                "{issue} already has an idea decision ({})",
                rec.state
            )));
        }
        if !matches!(rec.state.as_str(), "plan_ready" | "parked") {
            return Err(Error::rejected(format!(
                "{issue} is not at the operator gate (state {})",
                rec.state
            )));
        }
        if let Some(existing) = &rec.decision {
            let terminal = existing.action != "park";
            if existing.action == action || (terminal && matches!(action, "approve" | "reject")) {
                return Err(Error::rejected(format!(
                    "{issue} already has an idea decision ({})",
                    existing.action
                )));
            }
        }
        (
            rec.project.clone(),
            rec.tickets.clone(),
            rec.decision.clone(),
        )
    };
    let reason = reason.map(str::trim).filter(|s| !s.is_empty());
    match action {
        "approve" => {}
        "reject" => {
            let Some(reason) = &reason else {
                return Err(Error::rejected(
                    "a rejection needs a reason — the decision object records it",
                ));
            };
            if reason.len() > 2_000 {
                return Err(Error::rejected("rejection reason exceeds 2000 characters"));
            }
        }
        "park" => {
            let Some(date) = park_until.map(str::trim).filter(|s| !s.is_empty()) else {
                return Err(Error::rejected("park needs park_until as YYYY-MM-DD"));
            };
            if !valid_park_date(date) {
                return Err(Error::rejected("park_until must be a UTC date YYYY-MM-DD"));
            }
        }
        _ => {
            return Err(Error::rejected(
                "idea decision action must be approve, reject, or park",
            ))
        }
    }
    let mut children = prior
        .as_ref()
        .map(|d| d.children.clone())
        .unwrap_or_default();
    // The revision-bound write first: a stale view refuses here with
    // nothing created and nothing recorded.
    if action == "approve" {
        // CAD-756: the approval decision IS the done evidence — the
        // children minted below are the work's continuation.
        set_status_tags_if_rev(
            pm,
            issue,
            "done",
            Some("planned"),
            &["plan-ready", "parked", "idea-stale"],
            Some("idea plan approved — children minted"),
            Some(expect_rev),
        )?;
        children = create_children(pm, issue, &project, &proposed, &children)?;
    } else if action == "reject" {
        set_status_tags_if_rev(
            pm,
            issue,
            "dropped",
            None,
            &["plan-ready", "parked", "idea-stale"],
            None,
            Some(expect_rev),
        )?;
    } else {
        set_status_tags_if_rev(
            pm,
            issue,
            "review",
            Some("parked"),
            &["plan-ready"],
            None,
            Some(expect_rev),
        )?;
    }
    let decision = Decision {
        id: format!("idea-{}-{action}", issue.to_ascii_lowercase()),
        issue: issue.to_string(),
        project,
        action: action.to_string(),
        source: "operator".to_string(),
        recorded_via: "operator_connection".to_string(),
        actor: Some(actor.to_string()),
        reason: reason.map(str::to_string),
        park_until: park_until
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        children,
        at: time::iso(time::now_epoch()),
    };
    let rec = records.get_mut(issue).expect("record checked above");
    rec.state = match action {
        "approve" => "approved",
        "reject" => "rejected",
        _ => "parked",
    }
    .to_string();
    rec.decision = Some(decision.clone());
    Ok(json!({
        "duplicate": false,
        "decision": decision,
    }))
}

fn create_children(
    pm: &Pm,
    idea: &str,
    project: &str,
    proposed: &[Proposed],
    already: &[String],
) -> Result<Vec<String>> {
    let mut ids: Vec<String> = already.to_vec();
    let existing = board::load_all(&pm.dir, Some(project))?;
    for ticket in proposed {
        if existing
            .iter()
            .any(|iss| iss.front.parent.as_deref() == Some(idea) && iss.front.title == ticket.title)
            || ids.iter().any(|id| {
                existing
                    .iter()
                    .any(|iss| iss.front.id == *id && iss.front.title == ticket.title)
            })
        {
            if let Some(found) = existing.iter().find(|iss| {
                iss.front.parent.as_deref() == Some(idea) && iss.front.title == ticket.title
            }) {
                if !ids.contains(&found.front.id) {
                    ids.push(found.front.id.clone());
                }
            }
            continue;
        }
        let created = write::new_issue(
            pm,
            &pm.dir,
            Some(project),
            &ticket.title,
            Some("P3"),
            Some(idea),
            &[idea.to_string()],
            None,
            None,
            &[],
            None,
            None,
            "operator",
        )?;
        let id = created["id"]
            .as_str()
            .ok_or_else(|| Error::internal("new issue returned no id"))?
            .to_string();
        write::add_comment(
            pm,
            &id,
            &format!("Acceptance: {}", ticket.acceptance),
            Some("operator"),
            Some("acceptance"),
            None,
            "operator",
        )?;
        ids.push(id);
    }
    Ok(ids)
}
