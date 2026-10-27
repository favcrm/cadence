//! The current caller's PM issues, using the existing board identity and read model.
//! This is a personal filter, not a project-visibility or execution grant.
//!
//! The board reads one page at a time (`GET /api/my-work?tab=&q=&…`), and the
//! peek reads one issue's membership and assignment provenance
//! (`GET /api/my-work/issues/<ID>`). The hosted CLI keeps the whole list.

mod query;
use query::{Query, RowKey};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tiny_http::Request;

use crate::issue::board::{natural_key, View};
use crate::issue::line_times::LineTimes;
use crate::issue::{history, model, time, Pm};

use super::{operator, project_policy, read_model, HttpResp, ServeOpts};

/// How long the tracker history walk may take, and how many git reads the
/// before/after owner proofs may make. Past the bound the answer is `null` —
/// assignment provenance is never a refusal.
const PROVENANCE_WALK_TIMEOUT: Duration = Duration::from_secs(5);
const PROVENANCE_SHOW_BUDGET: usize = 128;
/// The cached line-times read behind a row's `updated` clock (the overview's
/// budget). Past it a row falls back to its own clocks: created and comments.
const LINE_TIMES_BUDGET: Duration = Duration::from_secs(3);
const PAGE_DEFAULT: usize = 50;
const PAGE_MAX: usize = 100;
const SEARCH_MAX_CHARS: usize = 200;
/// The Active tab's statuses, in the order its groups render.
const ACTIVE: &[&str] = &["doing", "review", "ready", "backlog"];

/// Keep the allowed read projection across the board's final live-session check.
pub(super) struct Snapshot {
    pm: Pm,
    readable: BTreeSet<String>,
    board: read_model::BoardRead,
}

impl Snapshot {
    /// The project-scoped board for `policy_actor`. The owner the read
    /// answers for is a session identity passed to each projection, never
    /// a request field.
    pub(super) fn load(state_dir: &Path, pm_dir: &Path, policy_actor: &str) -> crate::Result<Self> {
        let pm = Pm::at(pm_dir)?;
        let readable = project_policy::readable(&pm, policy_actor);
        let board = read_model::get(state_dir, pm_dir).board_readable(&pm, &readable);
        Ok(Self {
            pm,
            readable,
            board,
        })
    }

    /// A blocking read or session check may outlive a project grant.
    fn still_readable(&self, policy_actor: &str) -> bool {
        project_policy::readable(&self.pm, policy_actor) == self.readable
    }

    fn mine<'a>(&'a self, owner: &'a str) -> impl Iterator<Item = &'a View> + 'a {
        self.board
            .views
            .iter()
            .filter(move |view| view.issue.front.owner.as_deref() == Some(owner))
    }

    /// Every owned card with its assignment provenance — the hosted CLI's
    /// `my_work` answer. `subject` is the caller's verified platform `sub`.
    pub(super) fn payload(
        self,
        pm_dir: &Path,
        owner: &str,
        subject: Option<&str>,
        policy_actor: &str,
    ) -> Option<Value> {
        // Board order — provenance proofs spend the shared budget in this
        // same order every time.
        let owned: Vec<(String, &str)> = self.mine(owner).map(|view| (rel(view), owner)).collect();
        let mut assignments = history::owner_assignments(
            pm_dir,
            &owned,
            subject,
            PROVENANCE_WALK_TIMEOUT,
            PROVENANCE_SHOW_BUDGET,
        );
        if !self.still_readable(policy_actor) {
            return None;
        }
        let by_id = self.board.by_id();
        let ctx = self.board.ctx(&by_id);
        let mut body = json!({
            "owner": owner,
            "issues": self.mine(owner)
                .map(|view| {
                    let mut card = self.board.card(&ctx, view);
                    card["assignment"] = assignments.remove(&rel(view)).unwrap_or(Value::Null);
                    card
                })
                .collect::<Vec<_>>(),
        });
        read_model::merge_freshness(&mut body, &self.board.freshness);
        Some(body)
    }

    /// One page of the caller's issues for `query`, with per-tab counts
    /// over every filter except the tab.
    fn page(
        &self,
        owner: &str,
        query: &Query,
        times: Option<&LineTimes>,
        policy_actor: &str,
    ) -> Option<Value> {
        if !self.still_readable(policy_actor) {
            return None;
        }
        let mut projects = BTreeSet::new();
        let mut status_counts: BTreeMap<&str, usize> =
            model::STATUSES.iter().map(|status| (*status, 0)).collect();
        let mut rows: Vec<(RowKey, &View, Option<i64>)> = Vec::new();
        for view in self.mine(owner) {
            projects.insert(view.issue.project.as_str());
            if !query.matches(view) {
                continue;
            }
            if let Some(count) = status_counts.get_mut(view.status.as_str()) {
                *count += 1;
            }
            if query.tab.holds(&view.status) {
                let updated = updated_at(view, times);
                rows.push((query.key(view, updated), view, updated));
            }
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let total = rows.len();
        // Keyset paging: the next page starts after the last row shown, so a
        // row that moved since the previous page is neither repeated nor
        // skipped among the rows that did not.
        let start = query
            .after
            .as_ref()
            .map_or(0, |after| rows.partition_point(|row| row.0 <= *after));
        let end = start.saturating_add(query.limit).min(total);
        let active: usize = ACTIVE.iter().map(|status| status_counts[status]).sum();
        let done = status_counts["done"];
        let dropped = status_counts["dropped"];
        let mut body = json!({
            "owner": owner,
            "issues": rows[start..end]
                .iter()
                .map(|(_, view, updated)| row_json(view, *updated))
                .collect::<Vec<_>>(),
            "total": total,
            "next_cursor": (end < total).then(|| rows[end - 1].0.encode()),
            "counts": {
                "active": active,
                "done": done,
                "dropped": dropped,
                "all": status_counts.values().sum::<usize>(),
            },
            "status_counts": status_counts,
            "projects": projects,
        });
        read_model::merge_freshness(&mut body, &self.board.freshness);
        Some(body)
    }

    /// Whether `id` is the caller's, and its assignment provenance when it is.
    /// An issue that is not the caller's (or not readable) answers no
    /// provenance at all.
    fn membership(
        &self,
        pm_dir: &Path,
        owner: &str,
        subject: Option<&str>,
        id: &str,
    ) -> (bool, Value) {
        let Some(view) = self.mine(owner).find(|view| view.issue.front.id == id) else {
            return (false, Value::Null);
        };
        let key = rel(view);
        let assignment = history::owner_assignments(
            pm_dir,
            &[(key.clone(), owner)],
            subject,
            PROVENANCE_WALK_TIMEOUT,
            PROVENANCE_SHOW_BUDGET,
        )
        .remove(&key)
        .unwrap_or(Value::Null);
        (true, assignment)
    }
}

fn rel(view: &View) -> String {
    format!("{}/{}", view.issue.project, view.issue.front.id)
}

/// The issue's last recorded activity: its creation, newest comment and
/// the last change of its `status:` or `owner:` line.
fn updated_at(view: &View, times: Option<&LineTimes>) -> Option<i64> {
    let front = &view.issue.front;
    let lines = times.map(|times| {
        let project = view.issue.project.as_str();
        times
            .status_at(project, &front.id)
            .max(times.owner_at(project, &front.id))
    });
    view.issue
        .comments
        .iter()
        .filter_map(|comment| time::parse_iso(&comment.front.at))
        .chain(time::parse_iso(&front.created))
        .chain(lines.flatten())
        .max()
}

/// The list row: what a queue card renders, not the full board card.
fn row_json(view: &View, updated: Option<i64>) -> Value {
    let front = &view.issue.front;
    json!({
        "id": front.id,
        "project": view.issue.project,
        "title": front.title,
        "status": view.status,
        "priority": front.priority,
        "blocked": view.blocked,
        "blocked_reason": view.blocked_reason,
        "container": view.container,
        "checks": {"done": view.checks_done, "total": view.checks_total},
        "counts": {"comments": view.issue.comments.len()},
        "created": front.created,
        "updated": updated.map(time::iso),
    })
}

fn is_owner(caller: &operator::Caller) -> bool {
    matches!(caller, operator::Caller::Operator(_))
        || matches!(caller, operator::Caller::Named(named) if named.operator)
}

fn subject_of(caller: &operator::Caller) -> Option<&str> {
    match caller {
        operator::Caller::Named(named) => Some(named.sub.as_str()),
        _ => None,
    }
}

/// The human caller this read answers for and the spelling its project
/// policy reads. Agents are refused.
fn human_caller(
    request: &Request,
    state_dir: &Path,
    opts: &ServeOpts,
) -> Result<(operator::Caller, Cow<'static, str>), HttpResp> {
    let caller = operator::board_caller(request, state_dir, opts, false, None)?;
    if matches!(caller, operator::Caller::Agent(_)) {
        return Err(super::err_response(
            403,
            "My work requires a human board session",
        ));
    }
    let policy_actor = if is_owner(&caller) {
        Cow::Borrowed("operator")
    } else {
        Cow::Owned(operator::caller_spelling(&caller))
    };
    Ok((caller, policy_actor))
}

/// Tracker/daemon reads may await I/O. Never release the result after a
/// membership withdrawal or a caller change during that read.
fn recheck(
    request: &Request,
    state_dir: &Path,
    opts: &ServeOpts,
    caller: &operator::Caller,
) -> Result<(), HttpResp> {
    match operator::board_caller(request, state_dir, opts, false, None) {
        Ok(current)
            if !matches!(current, operator::Caller::Agent(_))
                && current.author() == caller.author()
                && is_owner(&current) == is_owner(caller)
                && subject_of(&current) == subject_of(caller) =>
        {
            Ok(())
        }
        Ok(_) => Err(super::err_response(
            403,
            "My work session changed; sign in again",
        )),
        Err(response) => Err(response),
    }
}

fn access_changed() -> HttpResp {
    super::err_response(403, "My work project access changed; refresh")
}

/// `GET /api/my-work` — one page of the caller's issues.
pub(super) fn get(
    request: &Request,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
    raw_query: &str,
) -> HttpResp {
    let query = match Query::parse(raw_query) {
        Ok(query) => query,
        Err(message) => return super::err_response(400, &message),
    };
    let (caller, policy_actor) = match human_caller(request, state_dir, opts) {
        Ok(found) => found,
        Err(response) => return response,
    };
    let snapshot = match Snapshot::load(state_dir, pm_dir, &policy_actor) {
        Ok(snapshot) => snapshot,
        Err(error) => return super::err_response(503, &error.to_string()),
    };
    let times = LineTimes::load(pm_dir, LINE_TIMES_BUDGET).ok();
    if let Err(response) = recheck(request, state_dir, opts, &caller) {
        return response;
    }
    match snapshot.page(caller.author(), &query, times.as_ref(), &policy_actor) {
        Some(body) => super::json_response(body),
        None => access_changed(),
    }
}

/// `GET /api/my-work/issues/<ID>` — whether the issue is still the caller's,
/// and the recorded handoff that made it so.
pub(super) fn get_issue(
    request: &Request,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
    raw_id: &str,
    raw_query: &str,
) -> HttpResp {
    if !raw_query.is_empty() {
        return super::err_response(400, "My work issue reads accept no query parameters");
    }
    let Ok(id) = model::check_id(raw_id) else {
        return super::err_response(400, "bad issue id");
    };
    let (caller, policy_actor) = match human_caller(request, state_dir, opts) {
        Ok(found) => found,
        Err(response) => return response,
    };
    let snapshot = match Snapshot::load(state_dir, pm_dir, &policy_actor) {
        Ok(snapshot) => snapshot,
        Err(error) => return super::err_response(503, &error.to_string()),
    };
    let owner = caller.author();
    let (assigned, assignment) = snapshot.membership(pm_dir, owner, subject_of(&caller), &id);
    if let Err(response) = recheck(request, state_dir, opts, &caller) {
        return response;
    }
    if !snapshot.still_readable(&policy_actor) {
        return access_changed();
    }
    super::json_response(json!({
        "owner": owner,
        "id": id,
        "assigned": assigned,
        "assignment": assignment,
    }))
}

#[cfg(test)]
mod tests {
    use super::query::{Sort, Tab};
    use super::*;

    #[test]
    fn query_admits_only_its_closed_parameter_set() {
        assert_eq!(Query::parse(""), Ok(Query::default()));
        let query = Query::parse(
            "tab=done&q=%20SMTP+Relay%20&project=agenticos-v2&priority=P1&blocked=1&sort=priority&cursor=0.1.-1700000000.CAD.-42&limit=100",
        )
        .unwrap();
        assert_eq!(
            query,
            Query {
                tab: Tab::Done,
                search: Some("smtp relay".to_string()),
                project: Some("agenticos-v2".to_string()),
                priority: Some("P1".to_string()),
                blocked: true,
                sort: Sort::Priority,
                after: Some(RowKey {
                    group: 0,
                    rank: 1,
                    newest: -1_700_000_000,
                    prefix: "CAD".to_string(),
                    number: -42,
                }),
                limit: 100,
            }
        );
        // A blank search is no search, not a match-nothing filter.
        assert_eq!(Query::parse("q=+").unwrap().search, None);
        for refused in [
            "owner=alpha",
            "subject=abc",
            "tab=active&tab=done",
            "tab=mine",
            "project=../x",
            "priority=P9",
            "blocked=0",
            "sort=owner",
            "cursor=50",
            "cursor=0.0.1.cad.-1",
            "cursor=0.0.1.CAD.-1.extra",
            "limit=0",
            "limit=101",
            "q=%ZZ",
        ] {
            assert!(Query::parse(refused).is_err(), "{refused} must be refused");
        }
        assert!(Query::parse(&format!("q={}", "a".repeat(SEARCH_MAX_CHARS + 1))).is_err());
        let key = RowKey {
            group: 2,
            rank: 0,
            newest: i64::MAX,
            prefix: "AOS".to_string(),
            number: -7,
        };
        assert_eq!(RowKey::decode(&key.encode()), Some(key));
    }

    fn ids(body: &Value) -> Vec<String> {
        body["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn pages_cover_each_row_once_and_stay_scoped_to_the_caller() {
        use crate::issue::write as issue_write;
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        issue_write::project_add(&pm, "alpha", "ALP", &[], &[], &[], None).unwrap();
        issue_write::project_add(&pm, "beta", "BET", &[], &[], &[], None).unwrap();
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let new = |project: &str, title: &str, owner: &str| {
            issue_write::new_issue(
                &pm,
                &pm.dir,
                Some(project),
                title,
                None,
                None,
                &[],
                Some(owner),
                None,
                &[],
                None,
                None,
                "operator",
            )
            .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let set = |id: &str, pairs: &[&str]| {
            let pairs: Vec<String> = pairs.iter().map(|pair| pair.to_string()).collect();
            let force = Some("fixture: done without merge evidence");
            issue_write::set_fields(&pm, &[id.to_string()], &pairs, "operator", force).unwrap();
        };
        let mut done = Vec::new();
        for n in 0..7 {
            let id = new("alpha", &format!("Finished task {n}"), "mira");
            set(&id, &["status=done"]);
            done.push(id);
        }
        let doing = new("beta", "Relay SMTP for tenants", "mira");
        set(&doing, &["status=doing", "priority=P0"]);
        let backlog = new("alpha", "Write the onboarding guide", "mira");
        let dropped = new("beta", "Old idea", "mira");
        set(&dropped, &["status=dropped"]);
        let theirs = new("alpha", "Someone else's SMTP work", "noor");

        let page = |raw: &str| {
            let snapshot = Snapshot::load(&state, &pm.dir, "operator").unwrap();
            let times = LineTimes::load(&pm.dir, LINE_TIMES_BUDGET).ok();
            snapshot
                .page(
                    "mira",
                    &Query::parse(raw).unwrap(),
                    times.as_ref(),
                    "operator",
                )
                .unwrap()
        };

        // Default: the Active tab only, doing before backlog, counts for every tab.
        let active = page("");
        assert_eq!(
            ids(&active),
            vec![doing.clone(), backlog.clone()],
            "{active}"
        );
        assert_eq!(
            active["counts"],
            json!({"active": 2, "done": 7, "dropped": 1, "all": 10})
        );
        assert_eq!(active["projects"], json!(["alpha", "beta"]));
        assert_eq!(active["next_cursor"], Value::Null);
        // The list row is the queue projection, not the full board card.
        assert!(active["issues"][0].get("refs").is_none(), "{active}");
        assert!(active["issues"][0].get("work").is_none(), "{active}");

        // Done in pages of 3: every done row once, then no cursor.
        let mut seen = Vec::new();
        let mut raw = "tab=done&limit=3".to_string();
        loop {
            let body = page(&raw);
            assert_eq!(body["total"], 7, "{body}");
            seen.extend(ids(&body));
            match body["next_cursor"].as_str() {
                Some(cursor) => raw = format!("tab=done&limit=3&cursor={cursor}"),
                None => break,
            }
        }
        let mut expected = done.clone();
        expected.sort();
        let mut sorted = seen.clone();
        sorted.sort();
        assert_eq!(sorted, expected, "each done row exactly once: {seen:?}");

        // A row that leaves the tab between pages does not shift the next page.
        let first = page("tab=done&limit=3");
        let cursor = first["next_cursor"].as_str().unwrap().to_string();
        set(&ids(&first)[0], &["status=doing"]);
        let second = page(&format!("tab=done&limit=3&cursor={cursor}"));
        assert_eq!(ids(&second), seen[3..6].to_vec(), "{second}");

        // Search, project, priority and blocked narrow server side; the
        // counts follow the filters and never include another owner.
        let smtp = page("tab=all&q=smtp");
        assert_eq!(ids(&smtp), vec![doing.clone()], "{smtp}");
        assert_eq!(smtp["counts"]["all"], 1);
        assert!(!ids(&page("tab=all&q=someone")).contains(&theirs));
        assert_eq!(
            ids(&page("tab=dropped&project=beta")),
            vec![dropped.clone()]
        );
        assert_eq!(ids(&page("priority=P0")), vec![doing.clone()]);
        assert!(ids(&page("tab=all&blocked=1")).is_empty());
    }
}
