//! "Since you left" (CAD-339): a compact, derived summary of what
//! happened on the tracker since a timestamp — plans proposed and
//! decided, tickets whose status moved, task reports filed, and every
//! question still open. Read-only: nothing is stored; the master can
//! post the rendered text into its thread (daemon RPC `master_summary`).
//! The scheduled daily digest is a later ticket.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{board, task_report, time, Pm};

/// Bound on the one `git log` that finds moved tickets.
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Rows per section in the rendered text; the JSON keeps them all.
const TEXT_ROWS: usize = 12;

/// `--since`: epoch seconds, `YYYY-MM-DDTHH:MM:SSZ`, or a look-back
/// `<n>m|h|d` from now.
pub fn parse_since(s: &str, now: i64) -> Result<i64> {
    let s = s.trim();
    if let Ok(epoch) = s.parse::<i64>() {
        return Ok(epoch);
    }
    if let Some(epoch) = time::parse_iso(s) {
        return Ok(epoch);
    }
    let unit = match s.chars().last() {
        Some('m') => 60,
        Some('h') => 3600,
        Some('d') => 86_400,
        _ => 0,
    };
    if unit > 0 {
        if let Ok(n) = s[..s.len() - 1].parse::<i64>() {
            if n >= 0 {
                return Ok(now - n * unit);
            }
        }
    }
    Err(Error::rejected(format!(
        "--since '{s}' — epoch seconds, YYYY-MM-DDTHH:MM:SSZ, or a look-back like 30m, 24h, 7d"
    )))
}

/// Ids of issues whose `status:` line changed in a tracker commit at or
/// after `since` — one bounded `git log`. A tracker without git (or a
/// log that times out) yields `None`, reported as unknown.
fn moved_ids(pm_dir: &Path, since: i64) -> Option<BTreeSet<String>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(pm_dir).args([
        "log",
        &format!("--since=@{since}"),
        "--format=",
        "--name-only",
        "-G",
        "^status:",
        "--",
        "*/issue.md",
    ]);
    let out = crate::proc::run_bounded(&mut cmd, GIT_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let parts: Vec<&str> = l.trim().split('/').collect();
                (parts.len() == 3 && parts[2] == "issue.md").then(|| parts[1].to_string())
            })
            .collect(),
    )
}

fn at_or_after(at: Option<&str>, since: i64) -> bool {
    at.and_then(time::parse_iso).is_some_and(|t| t >= since)
}

/// The summary since `since` (epoch seconds).
pub fn since(pm: &Pm, since: i64) -> Result<Value> {
    let views = board::views(&pm.config.notes_dir(), board::load_all(&pm.dir, None)?);
    let mut proposed = Vec::new();
    let mut decided = Vec::new();
    let mut reports = Vec::new();
    let mut open = Vec::new();
    for v in &views {
        let f = &v.issue.front;
        if let Some(plan) = &f.plan {
            let row = json!({
                "epic": f.id, "project": v.issue.project, "title": f.title,
                "state": plan.state, "proposed_by": plan.proposed_by,
                "proposed_at": plan.proposed_at, "decided_at": plan.decided_at,
                "tickets": plan.tickets.len(),
            });
            if at_or_after(Some(&plan.proposed_at), since) {
                proposed.push(row.clone());
            }
            if at_or_after(plan.decided_at.as_deref(), since) {
                decided.push(row);
            }
        }
        for r in task_report::list(&v.issue.dir, &f.id) {
            if r["error"].is_null() && at_or_after(r["at"].as_str(), since) {
                reports.push(json!({
                    "issue": f.id, "report": r["name"], "kind": r["kind"],
                    "agent": r["agent"], "at": r["at"],
                }));
            }
        }
        for q in task_report::open_questions(&v.issue.dir, &f.id) {
            open.push(json!({
                "issue": f.id, "title": f.title, "report": q["name"],
                "agent": q["agent"], "at": q["at"], "impact": q["impact"],
                "escalated": !q["escalation"].is_null(),
            }));
        }
    }
    let moved = moved_ids(&pm.dir, since).map(|ids| {
        views
            .iter()
            .filter(|v| ids.contains(&v.issue.front.id))
            .map(|v| {
                json!({
                    "issue": v.issue.front.id, "project": v.issue.project,
                    "title": v.issue.front.title, "status": v.status,
                    "owner": v.issue.front.owner,
                })
            })
            .collect::<Vec<_>>()
    });
    reports.sort_by(|a, b| a["at"].as_str().cmp(&b["at"].as_str()));
    let mut out = json!({
        "since": time::iso(since),
        "plans_proposed": proposed,
        "plans_decided": decided,
        "tickets_moved": moved,
        "reports": reports,
        "open_questions": open,
    });
    out["text"] = json!(render(&out));
    Ok(out)
}

fn rows(title: &str, items: &[Value], line: impl Fn(&Value) -> String) -> String {
    if items.is_empty() {
        return format!("{title}: none\n");
    }
    let mut s = format!("{title} ({}):\n", items.len());
    for it in items.iter().take(TEXT_ROWS) {
        s.push_str(&format!("- {}\n", line(it)));
    }
    if items.len() > TEXT_ROWS {
        s.push_str(&format!("- … {} more\n", items.len() - TEXT_ROWS));
    }
    s
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// The compact text a thread entry carries.
pub fn render(summary: &Value) -> String {
    let empty = vec![];
    let list = |k: &str| summary[k].as_array().unwrap_or(&empty).clone();
    let mut s = format!("Since {}:\n", text(&summary["since"]));
    s.push_str(&rows("Plans proposed", &list("plans_proposed"), |p| {
        format!(
            "{} {} — {} tickets, {} (by {})",
            text(&p["epic"]),
            text(&p["title"]),
            p["tickets"],
            text(&p["state"]),
            text(&p["proposed_by"])
        )
    }));
    s.push_str(&rows("Plans decided", &list("plans_decided"), |p| {
        format!(
            "{} {} — {}",
            text(&p["epic"]),
            text(&p["title"]),
            text(&p["state"])
        )
    }));
    match summary["tickets_moved"].as_array() {
        Some(moved) => s.push_str(&rows("Tickets moved", moved, |t| {
            format!(
                "{} {} → {}",
                text(&t["issue"]),
                text(&t["title"]),
                text(&t["status"])
            )
        })),
        None => s.push_str("Tickets moved: unknown (tracker history unreadable)\n"),
    }
    s.push_str(&rows("Reports filed", &list("reports"), |r| {
        format!(
            "{} {} by {} at {}",
            text(&r["issue"]),
            text(&r["kind"]),
            text(&r["agent"]),
            text(&r["at"])
        )
    }));
    s.push_str(&rows("Open questions", &list("open_questions"), |q| {
        format!(
            "{} from {} — {}{}",
            text(&q["issue"]),
            text(&q["agent"]),
            text(&q["impact"]),
            if q["escalated"] == true {
                " (with the operator)"
            } else {
                ""
            }
        )
    }));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_accepts_epoch_iso_and_lookback() {
        let now = 1_000_000;
        assert_eq!(parse_since("12345", now).unwrap(), 12345);
        assert_eq!(
            parse_since("2026-09-23T00:00:00Z", now).unwrap(),
            time::parse_iso("2026-09-23T00:00:00Z").unwrap()
        );
        assert_eq!(parse_since("30m", now).unwrap(), now - 1800);
        assert_eq!(parse_since("24h", now).unwrap(), now - 86_400);
        assert_eq!(parse_since("7d", now).unwrap(), now - 7 * 86_400);
        for bad in ["", "yesterday", "-3h", "3w", "2026-09-23"] {
            assert!(parse_since(bad, now).is_err(), "{bad}");
        }
    }

    #[test]
    fn render_names_every_section_and_caps_rows() {
        let many: Vec<Value> = (0..20)
            .map(|n| json!({"issue": format!("D-{n}"), "kind": "done", "agent": "w", "at": "t"}))
            .collect();
        let s = render(&json!({
            "since": "2026-09-23T00:00:00Z",
            "plans_proposed": [{"epic": "D-1", "title": "P", "tickets": 3,
                                "state": "approved", "proposed_by": "master"}],
            "plans_decided": [],
            "tickets_moved": null,
            "reports": many,
            "open_questions": [{"issue": "D-2", "agent": "w1", "impact": "blocks D-2",
                                "escalated": true}],
        }));
        assert!(s.contains("Plans proposed (1):\n- D-1 P — 3 tickets, approved (by master)"));
        assert!(s.contains("Plans decided: none"));
        assert!(s.contains("Tickets moved: unknown"));
        assert!(s.contains("Reports filed (20):"));
        assert!(s.contains("- … 8 more"));
        assert!(s.contains("D-2 from w1 — blocks D-2 (with the operator)"));
    }
}
