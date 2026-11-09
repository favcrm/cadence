//! CAD-2215: independent member job-event HTTP acceptance.
//!
//! Drives the real `POST /api/assist/job_events` route: a real daemon on a
//! Unix socket, the real board on a loopback port, the real platform authority
//! fixture, a real signed `agenticos-assistant-actor/1` envelope and the real
//! relay. A member's scoped page must arrive as opaque evidence — stable `id`
//! plus its own `cursor` on every event, no global `seq`, no internal
//! `_reader_scope` — while caller numeric/positional cursors and a caller
//! `cursor_format` override are refused. The relay's own
//! `member_job_events_relay_gate` is exercised directly for the mixed-version
//! case: a page without the exact scoped acknowledgement (a pre-CAD-2215
//! daemon's raw numeric feed included) is unavailable — never relayed, never
//! converted into an empty success.
//!
//! LIMITATION: the fixture runs a current daemon only. A pre-CAD-2215 daemon
//! process cannot be built from this tree, and no test-only daemon switch
//! exists (by design), so the "old daemon ACK" denial is proven at the real
//! relay gate plus the real current-daemon positive path rather than by
//! socketing a second, older binary.
//!
//! Key material lives in fixture temp dirs and the only setup operation is the
//! real operator `scoped_cursor_initialize` RPC. No production code or existing
//! acceptance is edited; only the module registration line was added.

use super::*;
use std::path::Path;

const HTTP_JOB_TITLE: &str = "CAD2215_HTTP_OPAQUE_JOB";
const HTTP_VISIBLE_MARKER: &str = "CAD2215_HTTP_VISIBLE_EVENT_";
const HTTP_PAYLOAD_SURVIVOR: &str = "CAD2215_HTTP_PAYLOAD_SURVIVOR";

fn write_visible_issue(pm: &Path) {
    let project = pm.join("visible");
    let issue = project.join("VIS-1");
    std::fs::create_dir_all(&issue).unwrap();
    std::fs::write(project.join("project.yaml"), "key: visible\nprefix: VIS\n").unwrap();
    std::fs::write(
        issue.join("issue.md"),
        "---\nid: VIS-1\ntitle: visible fixture\nstatus: backlog\n\
         priority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();
}

#[allow(clippy::type_complexity)] // one fixture channel pair, one shape
fn queue_observer() -> (
    Arc<dyn Fn(&str, &str) + Send + Sync>,
    std::sync::mpsc::Receiver<(String, String)>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let observer = Arc::new(move |request_id: &str, outcome: &str| {
        let _ = tx.send((request_id.to_string(), outcome.to_string()));
    });
    (observer, rx)
}

/// Accept a real member job request and wait for the joined promotion worker
/// to create exactly the planned canonical job. Returns the job id.
fn promoted_job(rig: &Rig, outcomes: &std::sync::mpsc::Receiver<(String, String)>) -> String {
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, response) = rig.call(
        "job_request",
        &envelope,
        &json!({
            "title": HTTP_JOB_TITLE,
            "request_key": "cad2215-http-opaque",
            "issue": "VIS-1",
        }),
    );
    assert_eq!(status, 200, "signed owner job request failed: {response}");
    let request_id = response["request_id"]
        .as_str()
        .expect("missing request receipt")
        .to_string();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .expect("the promotion worker did not settle this request in time");
        let (id, outcome) = outcomes
            .recv_timeout(remaining)
            .expect("promotion observer closed or timed out");
        if id != request_id {
            continue;
        }
        match outcome.as_str() {
            "created" => break,
            "pending" => continue,
            other => panic!("real request promotion failed: {other}"),
        }
    }
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, list) = rig.call("job_list", &envelope, &json!({}));
    assert_eq!(status, 200, "signed owner job list failed: {list}");
    let row = list["requests"]
        .as_array()
        .expect("missing owner request list")
        .iter()
        .find(|row| row["request_id"] == request_id)
        .unwrap_or_else(|| panic!("created request missing: {list}"));
    assert_eq!(row["status"], "created", "{row}");
    row["job"]
        .as_str()
        .expect("created request has no job id")
        .to_string()
}

/// The daemon's own event rows for this job, seeded as synthetic fixture data.
fn seed_events(rig: &Rig, job: &str) {
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    for index in 0..3 {
        db.execute(
            "INSERT INTO events(alias,kind,payload,job_id,task_id,at) VALUES(?,?,?,?,NULL,?)",
            rusqlite::params![
                "assist_ana",
                "fixture_progress",
                json!({"n": index, "marker": format!("{HTTP_VISIBLE_MARKER}{index}")}).to_string(),
                job,
                1000.0 + index as f64,
            ],
        )
        .unwrap();
    }
    // A visible event whose payload legitimately mentions `seq`/`event:<n>`:
    // the member page must preserve it verbatim and strip only the row's own
    // global position.
    db.execute(
        "INSERT INTO events(alias,kind,payload,job_id,task_id,at) VALUES(?,?,?,?,NULL,?)",
        rusqlite::params![
            "assist_ana",
            "fixture_progress",
            json!({"seq": 7, "text": "event:123", "marker": HTTP_PAYLOAD_SURVIVOR}).to_string(),
            job,
            1003.0,
        ],
    )
    .unwrap();
}

fn event_ids(page: &Value) -> Vec<String> {
    page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| {
            event["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| panic!("scoped HTTP event has no stable id: {event}"))
                .to_string()
        })
        .collect()
}

/// Only structural response metadata and each event row may be checked for a
/// global position; payloads are caller/provider content preserved verbatim.
fn assert_no_global_seq(value: &Value, what: &str) {
    let body = value
        .as_object()
        .unwrap_or_else(|| panic!("{what} is not an object: {value}"));
    for field in ["seq", "event_seq"] {
        assert!(
            !body.contains_key(field),
            "{what} exposed a structural {field}: {value}"
        );
    }
    for row in body
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let row = row
            .as_object()
            .unwrap_or_else(|| panic!("{what} row is not an object: {row}"));
        for field in ["seq", "event_seq"] {
            assert!(
                !row.contains_key(field),
                "{what} row exposed a structural {field}: {row:?}"
            );
        }
    }
}

fn error_code(body: &Value) -> Option<&str> {
    body["error"]["code"].as_str()
}

#[test]
fn member_job_events_http_relays_opaque_pages_and_refuses_positional_cursors() {
    let _flag = crate::daemon::team_assistants_refusal::AssistantFlagOn::arm();
    let (observer, outcomes) = queue_observer();
    let rig = start_with_queue_service(&["ana"], Some(observer), true);
    write_visible_issue(&rig.pm);
    let _keys = initialize_queue_keyring(&rig);
    let job = promoted_job(&rig, &outcomes);
    seed_events(&rig, &job);

    // The real member route answers with a scoped, opaque page.
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, page) = rig.call("job_events", &envelope, &json!({ "job": job }));
    assert_eq!(status, 200, "signed owner job events failed: {page}");
    assert_eq!(page["job"], json!(job), "{page}");
    let events = page["events"].as_array().expect("events array");
    assert!(
        events.len() >= 3,
        "the fixture events are missing from the scoped page: {page}"
    );
    let rendered = page.to_string();
    for index in 0..3 {
        let marker = format!("{HTTP_VISIBLE_MARKER}{index}");
        assert!(
            rendered.contains(marker.as_str()),
            "scoped page lost fixture event {index}: {page}"
        );
    }
    // Caller/provider content is relayed verbatim: the payload's own `seq` and
    // `event:<n>` survive, while the row's structural position does not.
    let survivor = events
        .iter()
        .find(|event| event["payload"]["marker"] == json!(HTTP_PAYLOAD_SURVIVOR))
        .unwrap_or_else(|| panic!("verbatim payload event missing: {page}"));
    assert_eq!(
        survivor["payload"]["seq"],
        json!(7),
        "the member page rewrote payload seq: {survivor}"
    );
    assert_eq!(
        survivor["payload"]["text"],
        json!("event:123"),
        "the member page rewrote a payload event reference: {survivor}"
    );
    assert!(
        !survivor.as_object().unwrap().contains_key("seq"),
        "the verbatim event row carried a structural seq: {survivor}"
    );
    for event in events {
        assert!(
            event["id"].as_str().is_some_and(|id| !id.is_empty()),
            "relayed event lost its stable id: {event}"
        );
        assert!(
            event["cursor"]
                .as_str()
                .is_some_and(|cursor| !cursor.is_empty()),
            "relayed event lost its own cursor: {event}"
        );
    }
    assert_no_global_seq(&page, "member job-event page");
    assert!(
        page.get(cadence_core::board_api::READER_SCOPE_FIELD)
            .is_none(),
        "the internal reader acknowledgement leaked through the board: {page}"
    );
    let cursor = page["cursor"]
        .as_str()
        .filter(|cursor| !cursor.is_empty())
        .expect("page did not end on the last visible cursor")
        .to_string();
    assert_eq!(
        Some(cursor.as_str()),
        events.last().and_then(|event| event["cursor"].as_str()),
        "page cursor is not the last visible event's cursor"
    );
    assert!(
        cursor.parse::<i64>().is_err(),
        "relayed cursor is numeric: {cursor}"
    );

    // Stable identity across a repeated read.
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, again) = rig.call("job_events", &envelope, &json!({ "job": job }));
    assert_eq!(status, 200, "{again}");
    assert_eq!(
        event_ids(&again),
        event_ids(&page),
        "re-reading changed stable ids"
    );
    assert_no_global_seq(&again, "re-read member page");

    // An empty poll echoes the supplied token and discloses nothing else.
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, empty) = rig.call(
        "job_events",
        &envelope,
        &json!({ "job": job, "after": cursor }),
    );
    assert_eq!(status, 200, "empty continue failed: {empty}");
    assert!(empty["events"].as_array().unwrap().is_empty(), "{empty}");
    assert_eq!(
        empty["cursor"],
        json!(cursor),
        "empty poll did not echo its token"
    );
    assert_no_global_seq(&empty, "empty member page");

    // A caller-supplied numeric position never reaches the daemon.
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, numeric) = rig.call("job_events", &envelope, &json!({ "job": job, "after": 1 }));
    assert_eq!(status, 400, "{numeric}");
    assert_eq!(
        error_code(&numeric),
        Some("job_events_cursor_numeric"),
        "{numeric}"
    );

    // A malformed token is one generic refusal — no position oracle.
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, malformed) = rig.call(
        "job_events",
        &envelope,
        &json!({ "job": job, "after": "csc1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" }),
    );
    assert_eq!(status, 400, "{malformed}");
    assert_eq!(
        error_code(&malformed),
        Some("job_events_cursor_invalid"),
        "{malformed}"
    );

    // The member can never select the numeric feed.
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, override_format) = rig.call(
        "job_events",
        &envelope,
        &json!({ "job": job, "cursor_format": "numeric" }),
    );
    assert_eq!(status, 400, "{override_format}");
    assert_eq!(
        error_code(&override_format),
        Some("job_events_cursor_format"),
        "{override_format}"
    );
}

#[test]
fn missing_or_foreign_scope_acknowledgement_is_unavailable_and_never_relayed() {
    use crate::ui::assist_route::member_job_events_relay_gate;

    let scoped_ack = cadence_core::board_api::JOB_EVENTS_SCOPE_ACK;
    let operator_ack = cadence_core::board_api::JOB_EVENTS_OPERATOR_SCOPE_ACK;

    // A pre-CAD-2215 daemon answer: a raw numeric feed with no reader scope at
    // all. It must not be relayed, and it must not collapse into an empty page.
    let mut legacy = json!({
        "events": [
            {"seq": 11, "alias": "assist_ana", "kind": "fixture_progress", "payload": {"n": 0}},
            {"seq": 12, "alias": "assist_ana", "kind": "fixture_progress", "payload": {"n": 1}},
        ],
        "cursor": 12,
    });
    assert!(
        !member_job_events_relay_gate(&mut legacy, None),
        "an unacknowledged numeric feed was relayable"
    );

    // The operator acknowledgement is a different reader mode: a member must
    // never receive the numeric document through the member route.
    let mut operator_mode = json!({
        "_reader_scope": operator_ack,
        "events": [{"seq": 11, "kind": "fixture_progress"}],
        "cursor": 11,
    });
    assert!(
        !member_job_events_relay_gate(&mut operator_mode, None),
        "the operator numeric mode was relayed as a member page"
    );

    // An acknowledged page that still carries a global sequence is malformed.
    let mut seq_leak = json!({
        "_reader_scope": scoped_ack,
        "events": [{"seq": 11, "id": "csrid_x", "cursor": "csc1_x"}],
        "cursor": "csc1_x",
    });
    assert!(
        !member_job_events_relay_gate(&mut seq_leak, None),
        "a scoped page carrying a global sequence was relayed"
    );

    // An acknowledged page with a numeric page cursor is a downgrade.
    let mut numeric_cursor = json!({
        "_reader_scope": scoped_ack,
        "events": [{"id": "csrid_x", "cursor": "csc1_y"}],
        "cursor": 12,
    });
    assert!(
        !member_job_events_relay_gate(&mut numeric_cursor, None),
        "a scoped page ending on a numeric position was relayed"
    );

    // An empty page that invents a cursor instead of echoing the supplied
    // token is malformed.
    let mut invented = json!({
        "_reader_scope": scoped_ack,
        "events": [],
        "cursor": "csc1_invented",
    });
    assert!(
        !member_job_events_relay_gate(&mut invented, Some("csc1_supplied")),
        "an empty page that did not echo the supplied token was relayed"
    );

    // The exact opaque shape is relayable, and the gate strips the internal
    // acknowledgement before the board's JSON response.
    let mut valid = json!({
        "_reader_scope": scoped_ack,
        "events": [{"id": "csrid_x", "cursor": "csc1_y", "kind": "fixture_progress"}],
        "cursor": "csc1_y",
    });
    assert!(
        member_job_events_relay_gate(&mut valid, None),
        "a well-formed scoped page was refused"
    );
    assert!(
        valid
            .get(cadence_core::board_api::READER_SCOPE_FIELD)
            .is_none(),
        "the gate did not strip the reader acknowledgement: {valid}"
    );

    // An initial empty page is legal with a null cursor.
    let mut initial_empty = json!({ "_reader_scope": scoped_ack, "events": [], "cursor": null });
    assert!(
        member_job_events_relay_gate(&mut initial_empty, None),
        "a legal initial empty page was refused"
    );
}
