//! CAD-2215: independent acceptance for scoped agent *activity*.
//!
//! A reader who may not read every project still reads the fleet: it sees the
//! agents, and it sees which of them are busy on work it can read. It must not
//! learn — from any scoped read — that an agent is busy only on work it cannot
//! read. The contract under test, stated as observable output:
//!
//! * a scoped `agent_list`/`agent_show` row's `state` is `"busy"` exactly when
//!   that alias has a running message inside the reader's readable coverage,
//!   and `null` otherwise — never the stored column, never `"idle"`;
//! * the scoped `states` filter is applied to that projected value: only
//!   `"busy"` is meaningful, and any other stored-state filter is REFUSED as
//!   unavailable (never answered with a false empty fleet);
//! * scoped `live` capacity is `null` — the stored fleet size is not knowable
//!   to this reader — while the operator keeps the numeric count;
//! * `dead`, `resumable` and `native_turn_steering_enabled` — each of which
//!   moves on hidden work — are absent from a scoped row;
//! * `tasks`, `lane_issues`, `visible_running_messages`, `messages` and
//!   `task_bindings` keep their narrowing: no hidden task, issue, message id or
//!   body;
//! * a native agent identity and an operator-proven `read_as` member read the
//!   same rows field-for-field;
//! * a proven operator still reads the stored state and the hidden work.
//!
//! Ordering is event-driven: the fixture commits every row before the read, and
//! the "hidden start" comparison is a byte comparison of the scoped documents
//! before and after the store mutation, never a timeout. No sleeps, no mocked
//! handlers, no production edits.

use super::*;
use crate::test_seam::{scoped, Asserted};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

const OPEN_ISSUE: &str = "VIS-1";
const HIDDEN_ISSUE: &str = "LOCK-1";
const OPEN_JOB: &str = "cad2215-activity-open-job";
const HIDDEN_JOB: &str = "cad2215-activity-hidden-job";
const OPEN_TASK: &str = "cad2215-activity-open-task";
const HIDDEN_TASK: &str = "cad2215-activity-hidden-task";
const OPEN_MESSAGE: &str = "cad2215-activity-open-message";
const HIDDEN_ON_OPEN_MESSAGE: &str = "cad2215-activity-hidden-on-open-message";
const HIDDEN_MESSAGE: &str = "cad2215-activity-hidden-message";
const VISIBLE_START_MESSAGE: &str = "cad2215-activity-visible-start-message";

const VIS_WORKER: &str = "cad2215-activity-vis-worker";
const HID_WORKER: &str = "cad2215-activity-hid-worker";
const READER: &str = "cad2215-activity-reader";
const MEMBER: &str = "cad2215-activity-member";

const OPEN_MARKER: &str = "CAD2215_ACTIVITY_OPEN_BODY";
const HIDDEN_MARKER: &str = "CAD2215_ACTIVITY_HIDDEN_BODY";

fn write_project(pm: &Path, key: &str, prefix: &str, issue: &str, policy: &str) {
    let project = pm.join(key);
    let issue_dir = project.join(issue);
    std::fs::create_dir_all(&issue_dir).unwrap();
    std::fs::write(
        project.join("project.yaml"),
        format!("key: {key}\nprefix: {prefix}\n{policy}"),
    )
    .unwrap();
    std::fs::write(
        issue_dir.join("issue.md"),
        format!(
            "---\nid: {issue}\ntitle: {issue} fixture\nstatus: backlog\n\
             priority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"
        ),
    )
    .unwrap();
}

struct World {
    _state: tempfile::TempDir,
    shared: Arc<Shared>,
}

fn insert_message(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    alias: &str,
    body: &str,
    task: &str,
    state: &str,
    created: f64,
) {
    tx.execute(
        "INSERT INTO messages(id,alias,body,sender,source,state,created,completed,task_id) \
         VALUES(?1,?2,?3,'operator','send',?4,?5,?5,?6)",
        rusqlite::params![id, alias, body, state, created, task],
    )
    .unwrap();
}

/// Two projects — `open` readable by everyone, `locked` private — and three
/// agents: one busy on read work, one busy only on unread work (`hid_mode`),
/// and the native reader itself. A message on the readable task belongs to the
/// visible worker so a private task on the same alias is exercised too.
fn build_world(hidden_worker_state: &str, hidden_running: bool) -> World {
    let state = tempfile::tempdir().unwrap();
    let pm = state.path().join("tracker");
    crate::issue::Pm::init(&pm).unwrap();
    write_project(&pm, "open", "VIS", OPEN_ISSUE, "");
    write_project(
        &pm,
        "locked",
        "LOCK",
        HIDDEN_ISSUE,
        "policy:\n  version: 1\n  visibility: private\n",
    );
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(state.path(), &opts).unwrap();
    for alias in [VIS_WORKER, HID_WORKER, READER] {
        register(&shared, state.path(), alias);
    }
    {
        let mut db = rusqlite::Connection::open(state.path().join("cadence.sqlite3")).unwrap();
        db.execute_batch(&format!(
            "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated) \
             VALUES('{OPEN_JOB}','Open','spec.md','{READER}','{OPEN_ISSUE}','open','open',1,1), \
                   ('{HIDDEN_JOB}','Hidden','spec.md','{READER}','{HIDDEN_ISSUE}','open','open',2,2); \
             INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) \
             VALUES('{OPEN_TASK}','{OPEN_JOB}','Open task','{VIS_WORKER}','running',1,1), \
                   ('{HIDDEN_TASK}','{HIDDEN_JOB}','Hidden task','{HID_WORKER}','running',2,2);"
        ))
        .unwrap();
        let tx = db.transaction().unwrap();
        // The visible worker also carries a running message on the private
        // task: its visible half must survive while the hidden half is unseen.
        insert_message(
            &tx,
            OPEN_MESSAGE,
            VIS_WORKER,
            OPEN_MARKER,
            OPEN_TASK,
            "running",
            10.0,
        );
        insert_message(
            &tx,
            HIDDEN_ON_OPEN_MESSAGE,
            VIS_WORKER,
            HIDDEN_MARKER,
            HIDDEN_TASK,
            "running",
            11.0,
        );
        if hidden_running {
            insert_message(
                &tx,
                HIDDEN_MESSAGE,
                HID_WORKER,
                HIDDEN_MARKER,
                HIDDEN_TASK,
                "running",
                12.0,
            );
        }
        tx.commit().unwrap();
    }
    {
        let db = rusqlite::Connection::open(state.path().join("cadence.sqlite3")).unwrap();
        for (alias, stored) in [
            (VIS_WORKER, "busy"),
            (HID_WORKER, hidden_worker_state),
            (READER, "idle"),
        ] {
            db.execute(
                "UPDATE agents SET state=?1 WHERE alias=?2",
                rusqlite::params![stored, alias],
            )
            .unwrap();
        }
    }
    World {
        _state: state,
        shared,
    }
}

fn call(shared: &Arc<Shared>, who: Asserted, method: &str, params: &Value) -> Result<Value> {
    let shared = Arc::clone(shared);
    scoped(who, || shared.dispatch(method, params, std::process::id()))
}

/// The member is modelled exactly as the board relays it: a proven operator
/// connection claiming a named user's read identity.
fn member(shared: &Arc<Shared>, method: &str, params: &Value) -> Value {
    call(shared, Asserted::Operator, method, params)
        .unwrap_or_else(|error| panic!("member {method} must be served: {error:?}"))
}

fn row(list: &Value, alias: &str) -> Value {
    list["agents"]
        .as_array()
        .unwrap_or_else(|| panic!("agent list has no agents array: {list}"))
        .iter()
        .find(|row| row["alias"] == json!(alias))
        .unwrap_or_else(|| panic!("agent {alias} is absent from the scoped list: {list}"))
        .clone()
}

fn row_aliases(list: &Value) -> Vec<String> {
    list["agents"]
        .as_array()
        .unwrap_or_else(|| panic!("agent list has no agents array: {list}"))
        .iter()
        .map(|row| {
            row["alias"]
                .as_str()
                .unwrap_or_else(|| panic!("agent row has no alias: {row}"))
                .to_string()
        })
        .collect()
}

fn strings(row: &Value, field: &str) -> Vec<String> {
    row[field]
        .as_array()
        .unwrap_or_else(|| panic!("agent row {field} is not an array: {row}"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("agent row {field} holds a non-string: {row}"))
                .to_string()
        })
        .collect()
}

fn scoped_list(shared: &Arc<Shared>, extra: Value) -> Value {
    let mut params = json!({"read_as": format!("users/{MEMBER}"), "board": true});
    for (key, value) in extra.as_object().into_iter().flatten() {
        params[key] = value.clone();
    }
    member(shared, "agent_list", &params)
}

/// The scoped reader's attempt to filter on a stored state the projection
/// cannot answer. The response must be a refusal, never a page.
fn scoped_filter_error(shared: &Arc<Shared>, extra: Value) -> Error {
    let mut params = json!({"read_as": format!("users/{MEMBER}"), "board": true});
    for (key, value) in extra.as_object().into_iter().flatten() {
        params[key] = value.clone();
    }
    call(shared, Asserted::Operator, "agent_list", &params)
        .expect_err("a scoped raw-state filter must be refused as unavailable")
}

fn assert_hidden_absent(text: &str, what: &str) {
    for secret in [
        HIDDEN_ISSUE,
        HIDDEN_TASK,
        HIDDEN_MESSAGE,
        HIDDEN_ON_OPEN_MESSAGE,
        HIDDEN_JOB,
        HIDDEN_MARKER,
        "locked",
        "Locked",
    ] {
        assert!(!text.contains(secret), "{what} disclosed {secret}: {text}");
    }
}

#[test]
fn scoped_agent_reads_report_only_project_visible_busy_work() {
    let world = build_world("busy", true);
    let shared = &world.shared;

    let list = scoped_list(shared, json!({}));
    let visible = row(&list, VIS_WORKER);
    let hidden = row(&list, HID_WORKER);

    // The visible worker is busy on a running message the reader may read.
    assert_eq!(
        visible["state"],
        json!("busy"),
        "visible running work is not busy: {visible}"
    );
    assert_eq!(
        strings(&visible, "visible_running_messages"),
        vec![OPEN_MESSAGE.to_string()],
        "the visible running message is not the only one exposed: {visible}"
    );
    assert_eq!(
        strings(&visible, "tasks"),
        vec![OPEN_TASK.to_string()],
        "the visible worker's task list is wrong: {visible}"
    );
    assert_eq!(
        strings(&visible, "lane_issues"),
        vec![OPEN_ISSUE.to_string()]
    );

    // The hidden-only worker is stored `busy` but reads as no work at all.
    assert_eq!(
        hidden["state"],
        Value::Null,
        "a hidden-only busy agent is observable as busy: {hidden}"
    );
    assert!(
        strings(&hidden, "visible_running_messages").is_empty(),
        "{hidden}"
    );
    assert!(strings(&hidden, "tasks").is_empty(), "{hidden}");
    assert!(strings(&hidden, "lane_issues").is_empty(), "{hidden}");

    // Fields that move on hidden work are not part of a scoped row.
    for field in ["dead", "resumable", "native_turn_steering_enabled"] {
        assert!(
            visible.get(field).is_none(),
            "scoped row still carries {field}: {visible}"
        );
        assert!(
            hidden.get(field).is_none(),
            "scoped row still carries {field}: {hidden}"
        );
    }

    assert_hidden_absent(&list.to_string(), "the scoped agent list");

    // Scoped capacity is unavailable: the stored fleet size is exactly what
    // this reader must not learn. A count of the rows it happens to see would
    // be a different, equally false answer.
    assert_eq!(
        list["live"],
        Value::Null,
        "scoped live must be unavailable, not a count: {list}"
    );

    // Only `busy` (work this reader can see running) is a meaningful filter.
    // A stored-state filter cannot be answered without reading the column that
    // moves on hidden work, so it is refused as unavailable — never a false
    // empty fleet.
    let busy = scoped_list(shared, json!({"states": ["busy"]}));
    assert_eq!(row_aliases(&busy), vec![VIS_WORKER.to_string()], "{busy}");
    assert_eq!(busy["live"], Value::Null, "{busy}");
    for stored in [
        "idle",
        "attention",
        "starting",
        "waiting_input",
        "stopping",
        "stopped",
        "offline",
    ] {
        let error = scoped_filter_error(shared, json!({"states": [stored]}));
        assert_eq!(
            error.kind(),
            "rejected",
            "scoped state filter [{stored}] was not a plain rejection: {error:?}"
        );
        assert!(
            error.code().is_none(),
            "scoped state filter [{stored}] gained a structured code: {error:?}"
        );
        let rendered = format!("{error:?}");
        for hidden in [HIDDEN_ISSUE, HIDDEN_TASK, HIDDEN_MESSAGE, HIDDEN_MARKER] {
            assert!(
                !rendered.contains(hidden),
                "scoped state filter [{stored}] leaked {hidden}: {rendered}"
            );
        }
        // The operator positive control below proves every one of these tokens
        // is valid vocabulary; only the scoped projection cannot answer them.
    }
    // A mixed list is the same single refusal — not a partial page.
    let mixed = scoped_filter_error(shared, json!({"states": ["busy", "idle"]}));
    assert_eq!(mixed.kind(), "rejected", "{mixed:?}");
    assert!(mixed.code().is_none(), "{mixed:?}");
    // An empty filter is no filter and must stay accepted.
    assert_eq!(
        scoped_list(shared, json!({"states": []})),
        list,
        "an empty scoped state filter changed the page"
    );

    // `agent_show` takes the same decision for the agent object it returns.
    let hidden_show = member(
        shared,
        "agent_show",
        &json!({"alias": HID_WORKER, "read_as": format!("users/{MEMBER}")}),
    );
    assert_eq!(
        hidden_show["agent"]["state"],
        Value::Null,
        "agent show exposed a hidden-only busy state: {hidden_show}"
    );
    assert!(
        hidden_show["messages"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{hidden_show}"
    );
    assert!(
        hidden_show["task_bindings"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{hidden_show}"
    );
    for field in ["dead", "resumable", "native_turn_steering_enabled"] {
        assert!(
            hidden_show["agent"].get(field).is_none(),
            "agent show still carries {field}: {hidden_show}"
        );
    }
    assert_hidden_absent(&hidden_show.to_string(), "member agent show");

    let visible_show = member(
        shared,
        "agent_show",
        &json!({"alias": VIS_WORKER, "read_as": format!("users/{MEMBER}")}),
    );
    assert_eq!(
        visible_show["agent"]["state"],
        json!("busy"),
        "{visible_show}"
    );
    let shown: Vec<&str> = visible_show["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("agent show has no messages: {visible_show}"))
        .iter()
        .filter_map(|message| message["id"].as_str())
        .collect();
    assert_eq!(
        shown,
        vec![OPEN_MESSAGE],
        "visible message missing or hidden one appended: {visible_show}"
    );
    let bound_tasks: Vec<&str> = visible_show["task_bindings"]
        .as_array()
        .unwrap_or_else(|| panic!("agent show has no task bindings: {visible_show}"))
        .iter()
        .filter_map(|binding| binding["id"].as_str())
        .collect();
    assert_eq!(bound_tasks, vec![OPEN_TASK], "{visible_show}");

    // An agent identity reads the same rows field-for-field.
    let native = call(
        shared,
        Asserted::Agent(READER.to_string()),
        "agent_list",
        &json!({"board": true}),
    )
    .expect("the native reader's agent list must be served");
    assert_eq!(
        row(&native, VIS_WORKER),
        visible,
        "native and member visible rows disagree"
    );
    assert_eq!(
        row(&native, HID_WORKER),
        hidden,
        "native and member hidden rows disagree"
    );
    assert_eq!(
        native["live"],
        Value::Null,
        "scoped live is not unavailable: {native}"
    );

    // Operator regression: stored state, numeric capacity and the raw state
    // filter are exactly as before — hidden work stays observable.
    let operator = call(shared, Asserted::Operator, "agent_list", &json!({}))
        .expect("operator agent list must be served");
    assert!(
        operator["live"].is_number(),
        "operator live capacity is no longer numeric: {operator}"
    );
    let operator_hidden = row(&operator, HID_WORKER);
    assert_eq!(operator_hidden["state"], json!("busy"), "{operator_hidden}");
    for field in ["dead", "resumable", "native_turn_steering_enabled"] {
        assert!(
            operator_hidden.get(field).is_some(),
            "operator row lost {field}: {operator_hidden}"
        );
    }
    assert_eq!(
        strings(&operator_hidden, "tasks"),
        vec![HIDDEN_TASK.to_string()]
    );
    let operator_idle = call(
        shared,
        Asserted::Operator,
        "agent_list",
        &json!({"states": ["idle"]}),
    )
    .expect("operator raw state filter must stay accepted");
    assert!(
        row_aliases(&operator_idle).contains(&READER.to_string()),
        "operator raw state filter changed meaning: {operator_idle}"
    );
    // Every token the scoped reader cannot answer is valid operator vocabulary:
    // the refusal above is about the reader's projection, not about the words.
    for stored in [
        "attention",
        "starting",
        "waiting_input",
        "stopping",
        "stopped",
        "offline",
    ] {
        let page = call(
            shared,
            Asserted::Operator,
            "agent_list",
            &json!({"states": [stored]}),
        )
        .unwrap_or_else(|error| panic!("operator filter [{stored}] was refused: {error:?}"));
        assert!(
            page["agents"].is_array(),
            "operator filter [{stored}]: {page}"
        );
    }
}

#[test]
fn hidden_only_start_is_invisible_to_scoped_agent_reads_but_operator_observes_it() {
    let world = build_world("idle", false);
    let shared = &world.shared;

    // Before: the hidden-only worker holds no running message and is stored
    // idle; the operator sees exactly that.
    let operator_before = call(shared, Asserted::Operator, "agent_list", &json!({}))
        .expect("operator agent list must be served");
    assert_eq!(row(&operator_before, HID_WORKER)["state"], json!("idle"));
    assert!(!operator_before.to_string().contains(HIDDEN_MESSAGE));

    let list_before = scoped_list(shared, json!({}));
    let busy_before = scoped_list(shared, json!({"states": ["busy"]}));
    let show_before = member(
        shared,
        "agent_show",
        &json!({"alias": HID_WORKER, "read_as": format!("users/{MEMBER}")}),
    );
    assert_eq!(row(&list_before, HID_WORKER)["state"], Value::Null);
    assert_eq!(row_aliases(&busy_before), vec![VIS_WORKER.to_string()]);
    assert_eq!(
        list_before["live"],
        Value::Null,
        "scoped live must already be unavailable: {list_before}"
    );

    // The hidden start, exactly as the store records it: the agent flips to
    // `busy` and one running message exists on the private task.
    {
        let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
        db.execute(
            "UPDATE agents SET state='busy' WHERE alias=?1",
            rusqlite::params![HID_WORKER],
        )
        .unwrap();
        db.execute(
            "INSERT INTO messages(id,alias,body,sender,source,state,created,completed,task_id) \
             VALUES(?1,?2,?3,'operator','send','running',20.0,20.0,?4)",
            rusqlite::params![HIDDEN_MESSAGE, HID_WORKER, HIDDEN_MARKER, HIDDEN_TASK],
        )
        .unwrap();
    }

    // The scoped documents are byte-identical: nothing moved.
    let list_after = scoped_list(shared, json!({}));
    let busy_after = scoped_list(shared, json!({"states": ["busy"]}));
    let show_after = member(
        shared,
        "agent_show",
        &json!({"alias": HID_WORKER, "read_as": format!("users/{MEMBER}")}),
    );
    assert_eq!(
        list_before, list_after,
        "a hidden-only start moved the scoped agent list"
    );
    assert_eq!(
        busy_before, busy_after,
        "a hidden-only start moved the scoped busy filter"
    );
    assert_eq!(
        show_before, show_after,
        "a hidden-only start moved the scoped agent show"
    );
    assert_hidden_absent(
        &list_after.to_string(),
        "the scoped agent list after a hidden start",
    );
    assert_hidden_absent(
        &show_after.to_string(),
        "member agent show after a hidden start",
    );

    // A VISIBLE start is observable as work, but scoped capacity stays
    // unavailable: `live` never becomes a number for this reader, and the
    // visible row is now busy with exactly its readable running messages.
    {
        let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
        db.execute(
            "INSERT INTO messages(id,alias,body,sender,source,state,created,completed,task_id) \
             VALUES(?1,?2,?3,'operator','send','running',21.0,21.0,?4)",
            rusqlite::params![VISIBLE_START_MESSAGE, VIS_WORKER, OPEN_MARKER, OPEN_TASK],
        )
        .unwrap();
    }
    let list_visible = scoped_list(shared, json!({}));
    assert_eq!(
        list_visible["live"],
        Value::Null,
        "a visible start made scoped capacity numeric: {list_visible}"
    );
    let visible_row = row(&list_visible, VIS_WORKER);
    assert_eq!(visible_row["state"], json!("busy"), "{visible_row}");
    let mut running = strings(&visible_row, "visible_running_messages");
    running.sort();
    let mut expected = vec![OPEN_MESSAGE.to_string(), VISIBLE_START_MESSAGE.to_string()];
    expected.sort();
    assert_eq!(
        running, expected,
        "visible work is no longer observable: {visible_row}"
    );

    // The operator still observes the start: stored state and hidden work.
    let operator_after = call(shared, Asserted::Operator, "agent_list", &json!({}))
        .expect("operator agent list must be served");
    assert!(
        operator_after["live"].is_number(),
        "operator live capacity is no longer numeric: {operator_after}"
    );
    let operator_row = row(&operator_after, HID_WORKER);
    assert_eq!(
        operator_row["state"],
        json!("busy"),
        "operator lost the stored state: {operator_row}"
    );
    assert_eq!(
        strings(&operator_row, "tasks"),
        vec![HIDDEN_TASK.to_string()]
    );
    assert_eq!(
        strings(&operator_row, "lane_issues"),
        vec![HIDDEN_ISSUE.to_string()]
    );
    let operator_show = call(
        shared,
        Asserted::Operator,
        "agent_show",
        &json!({"alias": HID_WORKER}),
    )
    .expect("operator agent show must be served");
    let operator_messages: Vec<&str> = operator_show["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("operator show has no messages: {operator_show}"))
        .iter()
        .filter_map(|message| message["id"].as_str())
        .collect();
    assert_eq!(
        operator_messages,
        vec![HIDDEN_MESSAGE],
        "operator lost hidden work: {operator_show}"
    );
    assert_eq!(
        operator_show["agent"]["state"],
        json!("busy"),
        "{operator_show}"
    );

    // The operator's numeric/states view keeps its legacy meaning.
    let operator_busy = call(
        shared,
        Asserted::Operator,
        "agent_list",
        &json!({"states": ["busy"]}),
    )
    .expect("operator filtered list must be served");
    let mut aliases = row_aliases(&operator_busy);
    aliases.sort();
    assert_eq!(
        aliases,
        vec![HID_WORKER.to_string(), VIS_WORKER.to_string()],
        "operator state filter lost a busy agent: {operator_busy}"
    );
}
