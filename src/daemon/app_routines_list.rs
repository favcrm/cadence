//! CAD-2402: the `routines.list` screen operation, a read over the
//! workspace's routines so an installed app can show them without reading
//! their content.
//!
//! The reply is exactly `{routines: [row]}`, at most [`ROWS_MAX`] rows ordered
//! by agent then name. A row is `{id, name, agent, schedule_text, status,
//! asks_first}` plus `next_run_at` (an RFC 3339 UTC instant) when the routine
//! has a next due time. It never carries markdown or script content, paths,
//! digests, inputs, the destination thread, the connection account or any run
//! output. Operator sessions only: the caller (`screen_service_execute`)
//! refuses a member before this runs.

use super::Shared;
use crate::error::Result;
use crate::store::routines::{schedule_text, Routine};
use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{json, Map, Value};

/// Rows one reply may carry.
const ROWS_MAX: usize = 500;
const NAME_MAX: usize = 200;
const AGENT_MAX: usize = 120;

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// `on`, `paused` or `repoint`, from the stored `on`, `paused` and
/// `repoint_required`. Anything else reads as paused: it is not running.
fn status_word(stored: &str) -> &'static str {
    match stored {
        "on" => "on",
        "repoint_required" => "repoint",
        _ => "paused",
    }
}

fn row(routine: &Routine, company: Tz) -> Value {
    let mut row = Map::new();
    row.insert("id".into(), json!(routine.id));
    row.insert("name".into(), json!(clip(&routine.name, NAME_MAX)));
    row.insert("agent".into(), json!(clip(&routine.agent_alias, AGENT_MAX)));
    row.insert(
        "schedule_text".into(),
        json!(schedule_text(routine, company)),
    );
    if let Some(at) = routine
        .next_due_at
        .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
    {
        row.insert(
            "next_run_at".into(),
            json!(at.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        );
    }
    row.insert("status".into(), json!(status_word(&routine.status)));
    row.insert(
        "asks_first".into(),
        json!(routine.checklist.iter().any(|step| step.requires_approval)),
    );
    Value::Object(row)
}

fn reply(mut routines: Vec<Routine>, company: Tz) -> Value {
    routines
        .sort_by(|a, b| (&a.agent_alias, &a.name, &a.id).cmp(&(&b.agent_alias, &b.name, &b.id)));
    let rows: Vec<Value> = routines
        .iter()
        .take(ROWS_MAX)
        .map(|routine| row(routine, company))
        .collect();
    json!({ "routines": rows })
}

impl Shared {
    /// The reply for one admitted `routines.list` call. The caller has
    /// already proved the live mount, the approved bundle, that the package
    /// declares the operation and that the session is an operator's.
    pub(super) fn screen_routines_list(&self) -> Result<Value> {
        let company = self
            .store
            .company_zone()
            .map(|(zone, _)| zone)
            .unwrap_or(Tz::UTC);
        Ok(reply(self.store.routine_list(None)?, company))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::routines::ChecklistStep;
    use std::collections::BTreeSet;

    const HK: Tz = chrono_tz::Asia::Hong_Kong;

    fn routine(id: &str, name: &str, agent: &str) -> Routine {
        Routine {
            id: id.into(),
            name: name.into(),
            agent_alias: agent.into(),
            trigger_kind: "schedule".into(),
            cron: None,
            interval_mins: None,
            timezone: Some("Asia/Hong_Kong".into()),
            event_source: None,
            target_kind: "script_then_markdown".into(),
            markdown_path: Some("routines/secret-path.md".into()),
            approved_rev: Some("rev-secret".into()),
            script_path: Some("routines/secret-script.sh".into()),
            approved_sha256: Some("digest-secret".into()),
            destination_thread: Some("thread-secret".into()),
            connection_account: Some("account-secret".into()),
            checklist: vec![],
            inputs: json!({"token": "input-secret"}),
            status: "on".into(),
            pause_reason: None,
            consecutive_failures: 0,
            next_due_at: None,
            last_run_at: Some(1),
            last_run_state: Some("output-secret".into()),
            created: 0.0,
            updated: 0.0,
        }
    }

    fn keys(row: &Value) -> BTreeSet<&str> {
        row.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    }

    /// Acceptance 2 and 5: two agents, a weekday 09:00 Hong Kong cron with an
    /// approval step and a 15-minute interval give exactly two rows with the
    /// documented key set; `asks_first` is true only on the first, the
    /// schedule is written in the company zone and `next_run_at` is an
    /// instant. Nothing the routine stores beyond the row leaks.
    #[test]
    fn two_routines_give_two_exact_rows_in_the_company_zone() {
        let mut weekly = routine("r-weekly", "Weekly outline", "alpha");
        weekly.cron = Some("0 9 * * 1-5".into());
        weekly.next_due_at = Some(1_795_395_600);
        weekly.checklist = vec![
            ChecklistStep {
                id: "s1".into(),
                label: "draft".into(),
                requires_approval: false,
                state: "pending".into(),
            },
            ChecklistStep {
                id: "s2".into(),
                label: "send".into(),
                requires_approval: true,
                state: "pending".into(),
            },
        ];
        let mut often = routine("r-often", "Inbox sweep", "beta");
        often.interval_mins = Some(15);
        often.status = "paused".into();

        // Handed over in the wrong order: the reply orders by agent.
        let out = reply(vec![often, weekly], HK);
        let rows = out["routines"].as_array().unwrap();
        assert_eq!(keys(&out), BTreeSet::from(["routines"]));
        assert_eq!(rows.len(), 2);

        let all = [
            "agent",
            "asks_first",
            "id",
            "name",
            "next_run_at",
            "schedule_text",
            "status",
        ];
        assert_eq!(keys(&rows[0]), BTreeSet::from(all));
        assert_eq!(rows[0]["id"], "r-weekly");
        assert_eq!(rows[0]["agent"], "alpha");
        assert_eq!(rows[0]["asks_first"], true);
        assert_eq!(rows[0]["status"], "on");
        assert_eq!(
            rows[0]["schedule_text"],
            "Weekdays at 09:00 (Asia/Hong_Kong)"
        );
        let at = rows[0]["next_run_at"].as_str().unwrap();
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(at)
                .unwrap()
                .timestamp(),
            1_795_395_600
        );
        assert!(at.ends_with('Z'), "{at}");

        // No next due time: the key is omitted, not null.
        let no_next: BTreeSet<&str> = all.into_iter().filter(|k| *k != "next_run_at").collect();
        assert_eq!(keys(&rows[1]), no_next);
        assert_eq!(rows[1]["asks_first"], false);
        assert_eq!(rows[1]["status"], "paused");
        assert_eq!(rows[1]["schedule_text"], "Every 15 min");

        let text = out.to_string();
        for leaked in [
            "secret",
            "routines/",
            "thread-secret",
            "account-secret",
            "digest",
            "input-secret",
            "output-secret",
        ] {
            assert!(!text.contains(leaked), "{leaked} in {text}");
        }
    }

    #[test]
    fn status_words_and_trigger_text_follow_the_stored_values() {
        assert_eq!(status_word("on"), "on");
        assert_eq!(status_word("paused"), "paused");
        assert_eq!(status_word("repoint_required"), "repoint");
        let mut event = routine("r-event", "On mail", "alpha");
        event.trigger_kind = "event".into();
        event.event_source = Some("mail.received".into());
        assert_eq!(schedule_text(&event, HK), "When mail.received");
        let mut cron = routine("r-cron", "Mondays", "alpha");
        cron.cron = Some("30 8 * * 1".into());
        cron.timezone = None;
        assert_eq!(schedule_text(&cron, HK), "Mon at 08:30 (Asia/Hong_Kong)");
    }

    #[test]
    fn rows_are_capped_and_text_is_bounded() {
        let many: Vec<Routine> = (0..ROWS_MAX + 5)
            .map(|n| routine(&format!("r{n:04}"), &"n".repeat(300), "alpha"))
            .collect();
        let out = reply(many, HK);
        let rows = out["routines"].as_array().unwrap();
        assert_eq!(rows.len(), ROWS_MAX);
        assert_eq!(rows[0]["name"].as_str().unwrap().chars().count(), NAME_MAX);
    }
}
