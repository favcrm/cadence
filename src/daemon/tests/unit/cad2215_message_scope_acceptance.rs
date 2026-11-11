//! CAD-2215: a scoped mailbox listing must not disclose the table-global
//! message sequence.
//!
//! Two equivalent authorized mail histories are built — one with private
//! work for a second, inaccessible mailbox interleaved between the reader's
//! rows. The reader's scoped `message_list` page, its continuation, and the
//! authorized identities it names must be the same in both, while the proven
//! operator keeps the numeric management view that shows the sequence moving.
//! The test seam asserts caller identities; it does not establish OS
//! isolation.

use super::*;
use crate::test_seam::{scoped, Asserted};

/// Fixed and later than every fixture agent's registration stamp, so the
/// terminal window depends only on the rows this test writes.
const CREATED: f64 = 4_102_444_800.0;

/// Two projects: `open` is workspace-visible, `locked` is private. A private
/// job in `locked` is work the reader cannot see; its messages still consume
/// table-global sequence positions.
fn mail_fixture(interleave: bool) -> (tempfile::TempDir, Arc<Shared>) {
    let temp = tempfile::tempdir().unwrap();
    let pm_dir = temp.path().join("tracker");
    crate::issue::Pm::init(&pm_dir).unwrap();
    for (key, prefix, policy) in [
        ("open", "OPEN", ""),
        (
            "locked",
            "LOCK",
            "policy:\n  version: 1\n  visibility: private\n",
        ),
    ] {
        let project = pm_dir.join(key);
        let issue = project.join(format!("{prefix}-1"));
        std::fs::create_dir_all(&issue).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: {key}\nprefix: {prefix}\n{policy}"),
        )
        .unwrap();
        std::fs::write(
            issue.join("issue.md"),
            format!(
                "---\nid: {prefix}-1\ntitle: {key} issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"
            ),
        )
        .unwrap();
    }

    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let shared = Shared::new(temp.path(), &opts).unwrap();
    register(&shared, temp.path(), "reader");
    register(&shared, temp.path(), "other");
    let conn = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated)
         VALUES ('open-job','Open','spec.md','reader','OPEN-1','fixture','open',1,1),
                ('locked-job','Locked','spec.md','other','LOCK-1','fixture','open',2,2);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES ('open-task','open-job','Open','reader','running',1,1),
                ('locked-task','locked-job','Locked','other','running',1,1);",
    )
    .unwrap();

    // Authorized mail for `reader`, with private work for the reader's own
    // mailbox and for `other` interleaved between those rows in the
    // interleaved history. The two histories differ only by that traffic.
    let insert = |id: &str, alias: &str, body: &str, task: &str| {
        conn.execute(
            "INSERT INTO messages(id,alias,body,sender,source,state,created,completed,task_id)
             VALUES (?1,?2,?3,'operator','send','completed',?4,?4,?5)",
            rusqlite::params![id, alias, body, CREATED, task],
        )
        .unwrap();
    };
    insert("m1", "reader", "opened-1", "open-task");
    if interleave {
        insert("h-other-1", "other", "hidden-other-one", "locked-task");
        insert("h-reader", "reader", "hidden-reader", "locked-task");
    }
    insert("m2", "reader", "opened-22", "open-task");
    if interleave {
        insert("h-other-2", "other", "hidden-other-two", "locked-task");
    }
    insert("m3", "reader", "opened-333", "open-task");
    (temp, shared)
}

fn ids(page: &Value) -> Vec<String> {
    page["messages"]
        .as_array()
        .expect("message list page")
        .iter()
        .map(|row| row["id"].as_str().expect("row identity").to_string())
        .collect()
}

fn seqs(page: &Value) -> Vec<i64> {
    page["messages"]
        .as_array()
        .expect("message list page")
        .iter()
        .map(|row| {
            row["seq"]
                .as_i64()
                .expect("operator view keeps the numeric sequence")
        })
        .collect()
}

/// No numeric sequence anywhere in a scoped response — not at the envelope
/// and not inside a row.
fn assert_no_global_seq(value: &Value) {
    match value {
        Value::Object(map) => {
            assert!(
                !map.contains_key("seq"),
                "scoped message list exposed a global sequence: {value}"
            );
            for child in map.values() {
                assert_no_global_seq(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                assert_no_global_seq(child);
            }
        }
        _ => {}
    }
}

#[test]
fn scoped_mailbox_listing_hides_the_global_message_sequence() {
    let (_control_temp, control) = mail_fixture(false);
    let (_interleaved_temp, interleaved) = mail_fixture(true);
    let pid = std::process::id();
    let reader = || Asserted::Agent("reader".to_string());

    // The seam must drive the real caller derivation, not an ambient identity.
    scoped(reader(), || {
        let caller = control.agent_caller(pid, "acceptance reader").unwrap();
        assert!(matches!(&caller, AgentCaller::Agent(alias) if alias == "reader"))
    });
    scoped(Asserted::Operator, || {
        let caller = interleaved
            .agent_caller(pid, "acceptance operator")
            .unwrap();
        assert!(matches!(caller, AgentCaller::Operator))
    });

    let reader_page = |shared: &Arc<Shared>, params: Value| {
        scoped(reader(), || shared.dispatch("message_list", &params, pid))
            .expect("reader lists its own mailbox")
    };
    let control_page = reader_page(&control, json!({"to": "reader", "limit": 3}));
    let interleaved_page = reader_page(&interleaved, json!({"to": "reader", "limit": 3}));

    for page in [&control_page, &interleaved_page] {
        assert_no_global_seq(page);
        assert_eq!(ids(page), vec!["m1", "m2", "m3"]);
    }
    // The visible pagination/count semantics must not move when private work
    // receives traffic; only the hidden numeric positions did.
    assert_eq!(ids(&control_page), ids(&interleaved_page));
    assert_eq!(control_page["omitted"], interleaved_page["omitted"]);
    assert_eq!(control_page["truncated"], interleaved_page["truncated"]);

    // Continuation uses the authorized id the page itself returned — no
    // numeric position is needed or offered.
    let anchor = ids(&interleaved_page)[0].clone();
    let control_resumed = reader_page(
        &control,
        json!({"to": "reader", "since": anchor.clone(), "limit": 3}),
    );
    let interleaved_resumed = reader_page(
        &interleaved,
        json!({"to": "reader", "since": anchor, "limit": 3}),
    );
    for page in [&control_resumed, &interleaved_resumed] {
        assert_no_global_seq(page);
        assert_eq!(ids(page), vec!["m2", "m3"]);
    }
    let last = ids(&interleaved_page)[1].clone();
    let tail = reader_page(
        &interleaved,
        json!({"to": "reader", "since": last, "limit": 3}),
    );
    assert_eq!(
        ids(&tail),
        vec!["m3"],
        "resume must not replay an earlier row"
    );

    // An inaccessible row is not a resume handle, whichever mailbox it is in,
    // and it is refused exactly as a nonexistent id is — no existence oracle.
    let absent = scoped(reader(), || {
        interleaved.dispatch(
            "message_list",
            &json!({"to": "reader", "since": "no-such-message"}),
            pid,
        )
    })
    .expect_err("a nonexistent message id must not resolve as a cursor")
    .to_string();
    assert!(
        !absent.contains("seq"),
        "cursor refusal leaked numeric ordering: {absent}"
    );
    for hidden in ["h-reader", "h-other-1"] {
        let refused = scoped(reader(), || {
            interleaved.dispatch(
                "message_list",
                &json!({"to": "reader", "since": hidden}),
                pid,
            )
        });
        let message = refused
            .expect_err("a hidden message id must not resolve as a cursor")
            .to_string();
        assert!(
            !message.contains("seq"),
            "cursor refusal leaked numeric ordering: {message}"
        );
        assert_eq!(
            message.replace(hidden, "no-such-message"),
            absent,
            "a hidden id is distinguishable from a nonexistent one"
        );
    }
    let foreign = scoped(reader(), || {
        interleaved.dispatch("message_list", &json!({"to": "other"}), pid)
    });
    assert!(
        foreign.is_err(),
        "a reader enumerated an inaccessible mailbox: {foreign:?}"
    );

    // The operator keeps the full numeric management view, including the
    // private mailbox and its hidden rows.
    let operator_list = |shared: &Arc<Shared>, to: &str| {
        scoped(Asserted::Operator, || {
            shared.dispatch("message_list", &json!({"to": to}), pid)
        })
        .expect("operator lists any mailbox")
    };

    let reader_view = operator_list(&interleaved, "reader");
    assert_eq!(ids(&reader_view), vec!["m1", "h-reader", "m2", "m3"]);
    let all_positions = seqs(&reader_view);
    assert!(
        all_positions.windows(2).all(|pair| pair[0] < pair[1]),
        "operator positions are not ordered: {all_positions:?}"
    );
    let authorized_positions = reader_view["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| matches!(row["id"].as_str(), Some("m1" | "m2" | "m3")))
        .map(|row| row["seq"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(authorized_positions.len(), 3);
    assert!(
        authorized_positions
            .windows(2)
            .any(|pair| pair[1] - pair[0] > 1),
        "interleaved traffic did not move the authorized positions: {authorized_positions:?}"
    );

    let other_view = operator_list(&interleaved, "other");
    assert_eq!(ids(&other_view), vec!["h-other-1", "h-other-2"]);
    let other_positions = seqs(&other_view);
    // The inaccessible mailbox's traffic sits between the reader's rows.
    assert!(other_positions[0] < authorized_positions[1]);
    assert!(authorized_positions[1] < other_positions[1]);
    assert!(other_positions[1] < authorized_positions[2]);

    let control_view = operator_list(&control, "reader");
    let control_positions = seqs(&control_view);
    assert_eq!(control_positions.len(), 3);
    assert!(
        control_positions
            .windows(2)
            .all(|pair| pair[1] - pair[0] == 1),
        "the control history has unexpected invisible traffic: {control_positions:?}"
    );
    assert_ne!(
        authorized_positions, control_positions,
        "the two histories must differ in the global sequence the reader is denied"
    );
}

/// The native detail read over the same interleaved history. A scoped
/// `agent_show` keeps the reader's authorized rows — identities, bodies and
/// order — but carries no message table position, and the hidden interleaved
/// row is not projected into the page at all. The proven operator's detail
/// over that history keeps the numeric positions, which is how this test
/// proves the hidden row really occupied one between two authorized rows.
#[test]
fn scoped_agent_detail_hides_the_global_message_sequence() {
    let (_temp, shared) = mail_fixture(true);
    let pid = std::process::id();
    let reader = || Asserted::Agent("reader".to_string());
    let shown = |who: Asserted, alias: &str| {
        scoped(who, || {
            shared.dispatch("agent_show", &json!({ "alias": alias }), pid)
        })
        .expect("the agent detail read must succeed for this caller")
    };

    // The seam must drive the handler's own caller derivation, so the guard
    // that chooses the projection is the real one for both identities.
    scoped(reader(), || {
        let caller = shared.agent_caller(pid, "agent show").unwrap();
        assert!(matches!(&caller, AgentCaller::Agent(alias) if alias == "reader"))
    });
    scoped(Asserted::Operator, || {
        assert!(matches!(
            shared.agent_caller(pid, "agent show").unwrap(),
            AgentCaller::Operator
        ))
    });

    // Scoped detail: only the reader's authorized rows, in their own order,
    // with their own bodies, and no row-level table position.
    let scoped_reader = shown(reader(), "reader");
    assert_eq!(ids(&scoped_reader), vec!["m1", "m2", "m3"]);
    let rows = scoped_reader["messages"]
        .as_array()
        .expect("scoped detail message rows");
    assert_eq!(rows.len(), 3);
    let bodies = rows
        .iter()
        .map(|row| {
            row["body"]
                .as_str()
                .expect("row content survives the projection")
        })
        .collect::<Vec<_>>();
    assert_eq!(bodies, vec!["opened-1", "opened-22", "opened-333"]);
    for row in rows {
        assert_eq!(
            row["alias"],
            json!("reader"),
            "the scoped detail lost its mailbox identity: {row}"
        );
        // The table position is row-level state, never content.
        assert!(
            row.get("seq").is_none(),
            "scoped detail exposed a message table position: {row}"
        );
    }

    // Inaccessible work in another mailbox is not in this alias' detail.
    let scoped_other = shown(reader(), "other");
    assert!(
        ids(&scoped_other).is_empty(),
        "scoped detail exposed another mailbox's hidden work: {scoped_other}"
    );

    // Operator detail: the same history, including the reader's own hidden
    // row, with the numeric positions the reader is denied.
    let operator_reader = shown(Asserted::Operator, "reader");
    assert_eq!(ids(&operator_reader), vec!["m1", "h-reader", "m2", "m3"]);
    let positions = seqs(&operator_reader);
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "operator positions are not ordered: {positions:?}"
    );
    let authorized_positions = operator_reader["messages"]
        .as_array()
        .expect("operator detail message rows")
        .iter()
        .filter(|row| matches!(row["id"].as_str(), Some("m1" | "m2" | "m3")))
        .map(|row| {
            row["seq"]
                .as_i64()
                .expect("operator detail keeps the numeric sequence")
        })
        .collect::<Vec<_>>();
    assert_eq!(authorized_positions.len(), 3);
    assert!(
        authorized_positions
            .windows(2)
            .all(|pair| pair[1] - pair[0] > 1),
        "the hidden row did not consume a position between authorized rows: \
         {authorized_positions:?}"
    );
    // The hidden row is the one occupying the gap between `m1` and `m2`.
    assert_eq!(
        operator_reader["messages"][1]["body"],
        json!("hidden-reader")
    );
    assert!(authorized_positions[0] < positions[1]);
    assert!(positions[1] < authorized_positions[1]);
}
