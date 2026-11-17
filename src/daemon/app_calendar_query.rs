//! CAD-2379: the `calendar.query` screen operation, a read over the
//! workspace's dated items so an installed app can show them without reading
//! another app's records.
//!
//! The reply is exactly `{entries, zone, sources}`. An entry carries a source,
//! an id, a title, a state, `needs_you`, and exactly one of `starts_at` (a UTC
//! instant) or `date` (an all-day `YYYY-MM-DD`). Titles carry no caption,
//! message text, customer list or recipient: a post is "Instagram post", a
//! routine is its own name, a due date is its own title. Each source is read
//! on its own; one that fails is reported `ok: false` and the others still
//! return, and nothing here widens what the caller could already see.
//!
//! Sources: social publish intents (`post`), routine slots and their runs
//! (`routine`), project milestone target dates and campaign work due dates
//! (`task`, all-day on the due date only). WhatsApp scheduled broadcasts
//! (`crm`) are not readable host-side without calling the remote door, so
//! `crm` is not attempted and is absent from `sources`.

use super::app_screen_services_rpc::ScreenServiceProof;
use super::Shared;
use crate::error::{Error, Result};
use crate::issue::work::{self, MilestoneStatus};
use crate::store::app_campaign_work::CampaignDue;
use crate::store::routines::{next_due_instant, Routine, RoutineRun};
use crate::store::social_publish::CalendarIntent;
use chrono::{NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

/// The widest window a call may ask for.
pub(super) const SPAN_MAX_DAYS: i64 = 62;
/// Entries one source may return. Three sources stay under the app parser's
/// 2000-entry reply bound; a source over this fails instead of truncating.
const SOURCE_MAX: usize = 600;
/// The reply bound the consuming app parser enforces.
const ENTRIES_MAX: usize = 2000;
/// A claimed post whose outcome is still unknown this long after its time.
const STALE_POST_SECS: i64 = 3600;
const TITLE_MAX: usize = 300;
const ID_MAX: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Window {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

/// `YYYY-MM-DD` exactly: four, two and two digits, a real calendar day.
fn parse_day(text: &str) -> Result<NaiveDate> {
    let b = text.as_bytes();
    let shaped = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    let bad = || Error::rejected("a calendar date is a real day written YYYY-MM-DD");
    if !shaped {
        return Err(bad());
    }
    NaiveDate::from_ymd_opt(
        text[0..4].parse().map_err(|_| bad())?,
        text[5..7].parse().map_err(|_| bad())?,
        text[8..10].parse().map_err(|_| bad())?,
    )
    .ok_or_else(bad)
}

/// The half-open window `[from, to)`: `to` after `from`, at most
/// [`SPAN_MAX_DAYS`] days.
pub(super) fn parse_window(from: &str, to: &str) -> Result<Window> {
    let (from, to) = (parse_day(from)?, parse_day(to)?);
    let span = (to - from).num_days();
    if span < 1 {
        return Err(Error::rejected("calendar `to` must be after `from`"));
    }
    if span > SPAN_MAX_DAYS {
        return Err(Error::rejected(format!(
            "a calendar window is at most {SPAN_MAX_DAYS} days"
        )));
    }
    Ok(Window { from, to })
}

/// The instant a day starts in `zone`. A midnight the zone skips falls to the
/// next valid instant after it; the last resort is UTC midnight.
fn day_start(zone: Tz, day: NaiveDate) -> i64 {
    let midnight = day.and_hms_opt(0, 0, 0).expect("midnight exists");
    match zone.from_local_datetime(&midnight) {
        chrono::LocalResult::Single(t) => t.timestamp(),
        chrono::LocalResult::Ambiguous(first, _) => first.timestamp(),
        chrono::LocalResult::None => zone
            .from_local_datetime(&(midnight + chrono::Duration::hours(1)))
            .earliest()
            .map_or_else(|| midnight.and_utc().timestamp(), |t| t.timestamp()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum When {
    At(i64),
    Day(NaiveDate),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    source: &'static str,
    id: String,
    when: When,
    title: String,
    state: String,
    needs_you: bool,
}

/// An id in the app parser's alphabet, whatever the stored parts hold.
fn entry_id(parts: &[&str]) -> String {
    let joined = parts.join(":");
    let mut id: String = joined
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '@' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    id.truncate(ID_MAX);
    id
}

/// A state in the app parser's alphabet: a lowercase word of `a-z0-9_-`.
fn state_word(raw: &str) -> String {
    let mut word: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !word.starts_with(|c: char| c.is_ascii_lowercase()) {
        word.insert(0, 'x');
    }
    word.truncate(64);
    word
}

fn title_of(raw: &str, fallback: &str) -> String {
    let trimmed = raw.trim();
    let text = if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    };
    text.chars().take(TITLE_MAX).collect()
}

impl Entry {
    fn to_json(&self) -> Value {
        let mut value = json!({
            "source": self.source,
            "id": self.id,
            "title": self.title,
            "state": self.state,
            "needs_you": self.needs_you,
        });
        match &self.when {
            When::At(epoch) => {
                value["starts_at"] = json!(chrono::DateTime::from_timestamp(*epoch, 0)
                    .unwrap_or_default()
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string());
            }
            When::Day(day) => value["date"] = json!(day.format("%Y-%m-%d").to_string()),
        }
        value
    }

    fn sort_key(&self, zone: Tz) -> (i64, &str) {
        let at = match &self.when {
            When::At(epoch) => *epoch,
            When::Day(day) => day_start(zone, *day),
        };
        (at, self.id.as_str())
    }
}

/// One source's outcome: what it returned and whether it read cleanly.
pub(super) struct SourceResult {
    source: &'static str,
    entries: Vec<Entry>,
    ok: bool,
}

impl SourceResult {
    fn from_result(source: &'static str, result: Result<Vec<Entry>>) -> Self {
        match result {
            Ok(entries) if entries.len() <= SOURCE_MAX => Self {
                source,
                entries,
                ok: true,
            },
            // Over its bound the source is reported failed and returns
            // nothing, instead of silently dropping the rest.
            Ok(_) | Err(_) => Self {
                source,
                entries: Vec::new(),
                ok: false,
            },
        }
    }
}

/// The reply: `{entries, zone, sources}` and nothing else. Entries sort by
/// time then id; a source that would push the reply past the bound the app
/// enforces is reported failed instead.
pub(super) fn assemble(zone: Tz, results: Vec<SourceResult>) -> Value {
    let mut entries: Vec<Entry> = Vec::new();
    let mut sources = Vec::new();
    for result in results {
        let fits = entries.len() + result.entries.len() <= ENTRIES_MAX;
        let ok = result.ok && fits;
        if fits {
            entries.extend(result.entries);
        }
        sources.push(json!({"source": result.source, "ok": ok}));
    }
    entries.sort_by(|a, b| a.sort_key(zone).cmp(&b.sort_key(zone)));
    json!({
        "entries": entries.iter().map(Entry::to_json).collect::<Vec<_>>(),
        "zone": zone.name(),
        "sources": sources,
    })
}

/// Publish intents as `post` entries. The title names the network only.
/// `needs_you` is a held post, and a claimed post whose outcome is still
/// unknown long after its time (`uncertain`).
pub(super) fn post_entries(rows: &[CalendarIntent], now: i64) -> Vec<Entry> {
    rows.iter()
        .map(|row| {
            let (state, needs_you) = match row.state.as_str() {
                "queued" => ("scheduled", false),
                "processing" if now - row.due_epoch > STALE_POST_SECS => ("uncertain", true),
                "processing" => ("publishing", false),
                "posted" => ("published", false),
                "refused" => ("refused", false),
                "held" => ("held", true),
                _ => ("unknown", false),
            };
            let title = match row.toolkit.as_str() {
                "instagram" => "Instagram post",
                "facebook" => "Facebook post",
                _ => "Social post",
            };
            Entry {
                source: "post",
                id: entry_id(&["post", &row.intent_id]),
                when: When::At(row.due_epoch),
                title: title.to_string(),
                state: state.to_string(),
                needs_you,
            }
        })
        .collect()
}

/// One routine with the facts its entries need.
pub(super) struct RoutineFacts {
    pub routine: Routine,
    pub thread_live: bool,
}

/// Routine slots and runs in `[from, to)` as `routine` entries. A run in the
/// window is its recorded state; a future slot in a routine that is on (or
/// waiting for a new thread) is expanded from its cron or interval in the
/// routine's own zone and takes its run's place when one exists at that
/// instant. `needs_you` is a routine whose thread is gone (`repoint_required`).
/// A schedule the engine cannot expand (no instant in the next year) adds no
/// slots; it is not a failed source. Past the per-source bound the whole
/// source fails.
pub(super) fn routine_entries(
    routines: &[RoutineFacts],
    runs: &[RoutineRun],
    from: i64,
    to: i64,
    now: i64,
) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut seen: HashSet<(String, i64)> = HashSet::new();
    let by_id: BTreeMap<&str, &RoutineFacts> = routines
        .iter()
        .map(|facts| (facts.routine.id.as_str(), facts))
        .collect();
    for run in runs {
        let Some(facts) = by_id.get(run.routine_id.as_str()) else {
            continue;
        };
        seen.insert((run.routine_id.clone(), run.due_instant));
        out.push(Entry {
            source: "routine",
            id: entry_id(&["routine", &run.routine_id, &run.due_instant.to_string()]),
            when: When::At(run.due_instant),
            title: title_of(&facts.routine.name, "Routine"),
            state: state_word(&run.state),
            needs_you: false,
        });
        if out.len() > SOURCE_MAX {
            return Err(Error::rejected("routine entries exceed the calendar bound"));
        }
    }
    for facts in routines {
        let routine = &facts.routine;
        let repoint = routine.status == "repoint_required"
            || (routine.destination_thread.is_some() && !facts.thread_live);
        if routine.trigger_kind != "schedule" || !(routine.status == "on" || repoint) {
            continue;
        }
        let zone = routine.timezone.as_deref().unwrap_or("UTC");
        // `next_due_instant` answers strictly after its argument, so a slot
        // exactly at the window start is reached from one second before.
        let mut after = (from - 1).max(now);
        while let Ok(slot) =
            next_due_instant(routine.cron.as_deref(), routine.interval_mins, zone, after)
        {
            if slot >= to {
                break;
            }
            after = slot;
            if seen.contains(&(routine.id.clone(), slot)) {
                continue;
            }
            out.push(Entry {
                source: "routine",
                id: entry_id(&["routine", &routine.id, &slot.to_string()]),
                when: When::At(slot),
                title: title_of(&routine.name, "Routine"),
                state: if repoint {
                    "repoint_required"
                } else {
                    "scheduled"
                }
                .to_string(),
                needs_you: repoint,
            });
            if out.len() > SOURCE_MAX {
                return Err(Error::rejected("routine entries exceed the calendar bound"));
            }
        }
    }
    Ok(out)
}

/// A milestone's target date as an all-day `task`, on that day only. The start
/// date is not shown; a cancelled milestone is not a due item.
pub(super) fn milestone_entries(
    milestones: &[(String, work::Milestone)],
    window: Window,
) -> Vec<Entry> {
    milestones
        .iter()
        .filter(|(_, m)| m.status != MilestoneStatus::Cancelled)
        .filter_map(|(project, m)| {
            let day = parse_day(m.target_date.as_deref()?).ok()?;
            (window.from <= day && day < window.to).then(|| Entry {
                source: "task",
                id: entry_id(&["milestone", project, &m.id]),
                when: When::Day(day),
                title: title_of(m.title.as_deref().unwrap_or(""), &m.id),
                state: m.status.as_str().to_string(),
                needs_you: false,
            })
        })
        .collect()
}

/// Campaign work due dates as all-day `task` entries on the due date only.
pub(super) fn campaign_entries(install: &str, rows: &[CampaignDue]) -> Vec<Entry> {
    rows.iter()
        .filter(|row| row.status.as_deref() != Some("cancelled"))
        .filter_map(|row| {
            let day = parse_day(&row.due_at).ok()?;
            Some(Entry {
                source: "task",
                id: entry_id(&["campaign", install, &row.context_id, &row.campaign_id]),
                when: When::Day(day),
                title: title_of(row.name.as_deref().unwrap_or(""), &row.campaign_id),
                state: state_word(row.status.as_deref().unwrap_or("planned")),
                needs_you: false,
            })
        })
        .collect()
}

impl Shared {
    /// The reply for one admitted `calendar.query` call. The caller has
    /// already proved the live mount, the approved bundle and that the
    /// package declares the operation.
    pub(super) fn screen_calendar_query(
        &self,
        proof: &ScreenServiceProof,
        params: &Value,
    ) -> Result<Value> {
        let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or("");
        let window = parse_window(text("from"), text("to"))?;
        // No workspace zone exists yet: the first social connection's, else UTC.
        let zone: Tz = self
            .store
            .app_binding_first_publish_zone()
            .ok()
            .flatten()
            .and_then(|name| name.parse().ok())
            .unwrap_or(Tz::UTC);
        let (from, to) = (day_start(zone, window.from), day_start(zone, window.to));
        let now = Utc::now().timestamp();

        let mut results = vec![SourceResult::from_result(
            "post",
            self.store
                .social_publish_calendar(from, to, SOURCE_MAX + 1)
                .map(|rows| post_entries(&rows, now)),
        )];
        // Routines are an operator surface: their native read admits the
        // operator or the master only. A member's call does not attempt the
        // source, so it is absent from `sources` rather than reported failed.
        if !proof.is_member() {
            results.push(SourceResult::from_result(
                "routine",
                self.calendar_routines(from, to, now),
            ));
        }
        results.push(self.calendar_tasks(proof, window));
        Ok(assemble(zone, results))
    }

    fn calendar_routines(&self, from: i64, to: i64, now: i64) -> Result<Vec<Entry>> {
        let mut facts = Vec::new();
        for routine in self.store.routine_list(None)? {
            let thread_live = self
                .store
                .destination_thread_live(routine.destination_thread.as_deref())?;
            facts.push(RoutineFacts {
                routine,
                thread_live,
            });
        }
        let runs = self.store.routine_runs_between(from, to, SOURCE_MAX + 1)?;
        routine_entries(&facts, &runs, from, to, now)
    }

    /// `task`: project milestones the caller may read, and campaign work due
    /// dates of the live sources. A half that cannot be read marks the source
    /// failed while the other half still returns.
    fn calendar_tasks(&self, proof: &ScreenServiceProof, window: Window) -> SourceResult {
        let mut entries = Vec::new();
        let mut ok = true;
        match self.calendar_milestones(proof, window) {
            Ok(found) => entries.extend(found),
            Err(_) => ok = false,
        }
        match self.calendar_campaign_work(window) {
            Ok((found, complete)) => {
                entries.extend(found);
                ok &= complete;
            }
            Err(_) => ok = false,
        }
        let mut result = SourceResult::from_result("task", Ok(entries));
        result.ok &= ok;
        result
    }

    fn calendar_milestones(
        &self,
        proof: &ScreenServiceProof,
        window: Window,
    ) -> Result<Vec<Entry>> {
        let pm = self.pm_at(&self.pm_dir()?)?;
        // The same project policy every other read applies; a project the
        // caller may not read contributes nothing.
        let readable = crate::project_policy::readable(&pm, proof.policy_actor());
        let mut found = Vec::new();
        for key in &readable {
            let (config, _) = work::load_config_or_default(&pm.dir, key);
            found.extend(config.milestones.into_iter().map(|m| (key.clone(), m)));
        }
        Ok(milestone_entries(&found, window))
    }

    /// The due dates of every live campaign-work source's live contexts, and
    /// whether the inventory was complete (a package that could not be
    /// inspected leaves it partial).
    fn calendar_campaign_work(&self, window: Window) -> Result<(Vec<Entry>, bool)> {
        let inventory = self.campaign_work_inventory()?;
        let mut complete = inventory["state"] == "ok";
        let (from, to) = (
            window.from.format("%Y-%m-%d").to_string(),
            window.to.format("%Y-%m-%d").to_string(),
        );
        let mut entries = Vec::new();
        for source in inventory["sources"].as_array().into_iter().flatten() {
            let Some(install) = source["install_id"].as_str() else {
                continue;
            };
            let live: HashSet<&str> = source["contexts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            // Read-only: a missing record file is no work, never a new file.
            let rows = match crate::store::app_records::RecordStore::open_readonly(
                &self.state_dir,
                install,
            ) {
                Ok(Some(records)) => records.app_campaign_work_due(&from, &to, SOURCE_MAX + 1),
                Ok(None) => continue,
                Err(_) => {
                    complete = false;
                    continue;
                }
            };
            match rows {
                Ok(rows) => {
                    let rows: Vec<CampaignDue> = rows
                        .into_iter()
                        .filter(|row| live.contains(row.context_id.as_str()))
                        .collect();
                    entries.extend(campaign_entries(install, &rows));
                }
                Err(_) => complete = false,
            }
        }
        Ok((entries, complete))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::work::Milestone;

    const HK: Tz = chrono_tz::Asia::Hong_Kong;

    fn day(text: &str) -> NaiveDate {
        parse_day(text).unwrap()
    }

    /// Mon 2026-11-16 to Mon 2026-11-23, the fixture week.
    fn week() -> Window {
        parse_window("2026-11-16", "2026-11-23").unwrap()
    }

    fn epoch(rfc3339: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .timestamp()
    }

    fn routine(id: &str, status: &str, cron: Option<&str>, tz: &str) -> Routine {
        Routine {
            id: id.into(),
            name: "Weekly outline".into(),
            agent_alias: "pm".into(),
            trigger_kind: "schedule".into(),
            cron: cron.map(str::to_string),
            interval_mins: None,
            timezone: Some(tz.into()),
            event_source: None,
            target_kind: "markdown".into(),
            markdown_path: None,
            approved_rev: None,
            script_path: None,
            approved_sha256: None,
            destination_thread: None,
            connection_account: None,
            checklist: vec![],
            inputs: json!({}),
            status: status.into(),
            pause_reason: None,
            consecutive_failures: 0,
            next_due_at: None,
            last_run_at: None,
            last_run_state: None,
            created: 0.0,
            updated: 0.0,
        }
    }

    fn run(routine_id: &str, due: i64, state: &str) -> RoutineRun {
        RoutineRun {
            id: format!("run-{due}"),
            routine_id: routine_id.into(),
            due_instant: due,
            trigger_source: "schedule".into(),
            state: state.into(),
            approved_rev: None,
            approved_sha256: None,
            checklist_state: vec![],
            script_output: None,
            message_id: None,
            error: None,
            started_at: 0.0,
            finished_at: None,
        }
    }

    #[test]
    fn window_grammar_and_bounds_refuse_everything_else() {
        assert!(parse_window("2026-11-16", "2026-11-23").is_ok());
        assert!(parse_window("2026-11-16", "2026-11-17").is_ok());
        // exactly 62 days is the widest window
        assert!(parse_window("2026-11-01", "2027-01-02").is_ok());
        for (from, to) in [
            ("2026-11-16", "2026-11-16"),
            ("2026-11-16", "2026-11-15"),
            ("2026-11-01", "2027-01-03"),
            ("2026-1-5", "2026-11-23"),
            ("2026-02-30", "2026-03-05"),
            ("20261116", "2026-11-23"),
            ("2026-11-16T00:00:00Z", "2026-11-23"),
            ("2026-11-16", ""),
            (" 2026-11-16", "2026-11-23"),
            ("２０２６-11-16", "2026-11-23"),
        ] {
            assert!(parse_window(from, to).is_err(), "{from:?}..{to:?}");
        }
    }

    #[test]
    fn a_one_week_fixture_returns_typed_entries_for_every_source() {
        let fri = day("2026-11-20");
        let posts = post_entries(
            &[CalendarIntent {
                intent_id: "spi_1".into(),
                state: "queued".into(),
                due_epoch: epoch("2026-11-18T02:00:00Z"),
                toolkit: "instagram".into(),
            }],
            epoch("2026-11-16T00:00:00Z"),
        );
        let routines = routine_entries(
            &[RoutineFacts {
                routine: routine("rt_1", "on", Some("0 9 * * 1"), "Asia/Hong_Kong"),
                thread_live: true,
            }],
            &[],
            day_start(HK, week().from),
            day_start(HK, week().to),
            epoch("2026-11-15T00:00:00Z"),
        )
        .unwrap();
        let tasks = milestone_entries(
            &[(
                "CAD".into(),
                Milestone {
                    id: "beta".into(),
                    title: Some("Beta".into()),
                    start_date: Some("2026-11-02".into()),
                    target_date: Some(fri.to_string()),
                    ..Milestone::default()
                },
            )],
            week(),
        );
        let reply = assemble(
            HK,
            vec![
                SourceResult::from_result("post", Ok(posts)),
                SourceResult::from_result("routine", Ok(routines)),
                SourceResult::from_result("task", Ok(tasks)),
            ],
        );
        let mut keys: Vec<_> = reply.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["entries", "sources", "zone"]);
        assert_eq!(reply["zone"], "Asia/Hong_Kong");
        assert_eq!(
            reply["sources"],
            json!([
                {"source": "post", "ok": true},
                {"source": "routine", "ok": true},
                {"source": "task", "ok": true},
            ])
        );
        let entries = reply["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        // Monday 09:00 in Hong Kong is 01:00Z, before the Wednesday post.
        assert_eq!(entries[0]["source"], "routine");
        assert_eq!(entries[0]["starts_at"], "2026-11-16T01:00:00Z");
        assert_eq!(entries[1]["source"], "post");
        assert_eq!(entries[1]["starts_at"], "2026-11-18T02:00:00Z");
        assert_eq!(entries[1]["title"], "Instagram post");
        assert_eq!(entries[1]["state"], "scheduled");
        // A work item due Friday is all-day on Friday and carries no instant.
        assert_eq!(entries[2]["source"], "task");
        assert_eq!(entries[2]["date"], "2026-11-20");
        assert!(entries[2].get("starts_at").is_none());
        for entry in entries {
            let mut keys: Vec<_> = entry.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            assert!(keys.iter().all(|k| matches!(
                k.as_str(),
                "source" | "id" | "title" | "state" | "needs_you" | "starts_at" | "date"
            )));
            assert!(entry["id"].as_str().unwrap().len() <= ID_MAX);
        }
    }

    #[test]
    fn a_failing_source_reports_not_ok_while_the_others_still_return() {
        let posts = post_entries(
            &[CalendarIntent {
                intent_id: "spi_1".into(),
                state: "held".into(),
                due_epoch: epoch("2026-11-18T02:00:00Z"),
                toolkit: "facebook".into(),
            }],
            epoch("2026-11-16T00:00:00Z"),
        );
        let reply = assemble(
            Tz::UTC,
            vec![
                SourceResult::from_result("post", Ok(posts)),
                SourceResult::from_result("routine", Err(Error::internal("store unavailable"))),
            ],
        );
        assert_eq!(
            reply["sources"],
            json!([
                {"source": "post", "ok": true},
                {"source": "routine", "ok": false},
            ])
        );
        assert_eq!(reply["entries"].as_array().unwrap().len(), 1);
        assert_eq!(reply["entries"][0]["state"], "held");
        assert_eq!(reply["entries"][0]["needs_you"], true);
    }

    #[test]
    fn a_source_over_its_bound_fails_instead_of_truncating() {
        let rows: Vec<CalendarIntent> = (0..=SOURCE_MAX)
            .map(|n| CalendarIntent {
                intent_id: format!("spi_{n}"),
                state: "queued".into(),
                due_epoch: 1_800_000_000 + n as i64,
                toolkit: "instagram".into(),
            })
            .collect();
        let reply = assemble(
            Tz::UTC,
            vec![SourceResult::from_result(
                "post",
                Ok(post_entries(&rows, 0)),
            )],
        );
        assert_eq!(reply["sources"], json!([{"source": "post", "ok": false}]));
        assert!(reply["entries"].as_array().unwrap().is_empty());
    }

    #[test]
    fn the_window_is_cut_in_the_reply_zone() {
        // 23:30Z on the 16th is already Tuesday the 17th in Hong Kong, so a
        // Hong Kong window starting the 17th holds it and the 16th does not.
        let instant = epoch("2026-11-16T23:30:00Z");
        let tue = day_start(HK, day("2026-11-17"));
        let wed = day_start(HK, day("2026-11-18"));
        assert!(tue <= instant && instant < wed);
        assert_eq!(tue, epoch("2026-11-16T16:00:00Z"));
    }

    #[test]
    fn post_states_and_needs_you_follow_the_publish_state() {
        let now = epoch("2026-11-18T12:00:00Z");
        let due = epoch("2026-11-18T02:00:00Z");
        let state = |s: &str, due_epoch: i64| {
            let e = &post_entries(
                &[CalendarIntent {
                    intent_id: "x".into(),
                    state: s.into(),
                    due_epoch,
                    toolkit: "instagram".into(),
                }],
                now,
            )[0];
            (e.state.clone(), e.needs_you)
        };
        assert_eq!(state("queued", due), ("scheduled".into(), false));
        assert_eq!(state("posted", due), ("published".into(), false));
        assert_eq!(state("held", due), ("held".into(), true));
        assert_eq!(state("processing", now - 60), ("publishing".into(), false));
        assert_eq!(state("processing", due), ("uncertain".into(), true));
    }

    #[test]
    fn routine_slots_expand_in_the_routine_zone_and_runs_take_their_slot() {
        let (from, to) = (day_start(HK, week().from), day_start(HK, week().to));
        let now = epoch("2026-11-15T00:00:00Z");
        // Daily at 09:00 Hong Kong, 01:00Z; the Monday slot already ran.
        let facts = [RoutineFacts {
            routine: routine("rt_1", "on", Some("0 9 * * *"), "Asia/Hong_Kong"),
            thread_live: true,
        }];
        let monday = epoch("2026-11-16T01:00:00Z");
        let entries =
            routine_entries(&facts, &[run("rt_1", monday, "succeeded")], from, to, now).unwrap();
        assert_eq!(entries.len(), 7);
        let first = entries
            .iter()
            .find(|e| e.when == When::At(monday))
            .expect("monday");
        assert_eq!(first.state, "succeeded");
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.when == When::At(monday))
                .count(),
            1,
            "a run and its slot are one entry"
        );
        // A slot exactly at the window start is included.
        let at_start = routine_entries(
            &[RoutineFacts {
                routine: routine("rt_2", "on", Some("0 0 * * *"), "Asia/Hong_Kong"),
                thread_live: true,
            }],
            &[],
            from,
            from + 3600,
            now,
        )
        .unwrap();
        assert_eq!(at_start.len(), 1);
        // Paused and waiting-approval routines expand no slots.
        for status in ["paused", "waiting_approval"] {
            let none = routine_entries(
                &[RoutineFacts {
                    routine: routine("rt_3", status, Some("0 9 * * *"), "UTC"),
                    thread_live: true,
                }],
                &[],
                from,
                to,
                now,
            )
            .unwrap();
            assert!(none.is_empty(), "{status}");
        }
    }

    #[test]
    fn a_routine_without_a_live_thread_needs_you() {
        let mut gone = routine("rt_4", "on", Some("0 9 * * *"), "UTC");
        gone.destination_thread = Some("thr_gone".into());
        let (from, to) = (
            day_start(Tz::UTC, week().from),
            day_start(Tz::UTC, week().to),
        );
        let entries = routine_entries(
            &[RoutineFacts {
                routine: gone,
                thread_live: false,
            }],
            &[],
            from,
            to,
            from - 86_400,
        )
        .unwrap();
        assert!(!entries.is_empty());
        assert!(entries
            .iter()
            .all(|e| e.needs_you && e.state == "repoint_required"));
    }

    #[test]
    fn expanding_past_the_bound_fails_the_source() {
        // Every fifteen minutes for a week is 672 slots, over the bound.
        let mut busy = routine("rt_5", "on", None, "UTC");
        busy.interval_mins = Some(15);
        let (from, to) = (
            day_start(Tz::UTC, week().from),
            day_start(Tz::UTC, week().to),
        );
        let result = routine_entries(
            &[RoutineFacts {
                routine: busy,
                thread_live: true,
            }],
            &[],
            from,
            to,
            from - 1,
        );
        assert!(result.is_err());
    }

    #[test]
    fn tasks_are_all_day_on_the_due_date_only() {
        let window = week();
        let milestones = [
            (
                "CAD".to_string(),
                Milestone {
                    id: "in".into(),
                    target_date: Some("2026-11-20".into()),
                    start_date: Some("2026-11-01".into()),
                    ..Milestone::default()
                },
            ),
            (
                "CAD".to_string(),
                Milestone {
                    id: "end-exclusive".into(),
                    target_date: Some("2026-11-23".into()),
                    ..Milestone::default()
                },
            ),
            (
                "CAD".to_string(),
                Milestone {
                    id: "start-only".into(),
                    start_date: Some("2026-11-18".into()),
                    ..Milestone::default()
                },
            ),
            (
                "CAD".to_string(),
                Milestone {
                    id: "cancelled".into(),
                    target_date: Some("2026-11-18".into()),
                    status: MilestoneStatus::Cancelled,
                    ..Milestone::default()
                },
            ),
        ];
        let found = milestone_entries(&milestones, window);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].when, When::Day(day("2026-11-20")));
        assert_eq!(found[0].title, "in");
        let campaign = campaign_entries(
            "ins_1",
            &[CampaignDue {
                context_id: "ctx_1".into(),
                campaign_id: "cmp_1".into(),
                due_at: "2026-11-19".into(),
                status: Some("in_progress".into()),
                name: Some("Autumn offer".into()),
            }],
        );
        assert_eq!(campaign[0].when, When::Day(day("2026-11-19")));
        assert_eq!(campaign[0].title, "Autumn offer");
        assert_eq!(campaign[0].state, "in_progress");
    }

    #[test]
    fn ids_states_and_titles_stay_in_the_app_alphabet() {
        let id = entry_id(&["campaign", "ins 1", "ctx/é", &"x".repeat(400)]);
        assert!(id.len() <= ID_MAX);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.:@-".contains(c)));
        assert_eq!(state_word("Skipped Stale"), "skipped_stale");
        assert_eq!(state_word("9x"), "x9x");
        assert_eq!(title_of("   ", "Fallback"), "Fallback");
        assert_eq!(title_of(&"t".repeat(500), "F").chars().count(), TITLE_MAX);
    }
}
