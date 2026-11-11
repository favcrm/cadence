//! CAD-2215: a scoped task detail must not disclose the table-global
//! message or verdict sequence.
//!
//! A private project's work is interleaved between a readable project's
//! messages and verdicts, so the readable rows' table-global positions are
//! not contiguous. A scoped reader's `task_show` — its message rows, its
//! `latest_verdict` and its `verdicts` history — must carry the authorized
//! content, order and identities with no row-level `seq`, while the proven
//! operator keeps the numeric management view that shows the sequence
//! moving. The test seam asserts caller identities; it does not establish
//! OS isolation.

use super::*;
use crate::test_seam::{scoped, Asserted};

/// Fixed and later than every fixture agent's registration stamp, so the
/// terminal window depends only on the rows this test writes.
const CREATED: f64 = 4_102_444_800.0;

/// Two projects: `open` is workspace-visible, `locked` is private. Work in
/// `locked` still consumes table-global message and verdict positions.
fn detail_fixture() -> (tempfile::TempDir, Arc<Shared>) {
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
         INSERT INTO tasks(id,job_id,title,assignee,state,revision,head_sha,created,updated)
         VALUES ('open-task','open-job','Open task','reader','review',2,'sha-2',1,1),
                ('locked-task','locked-job','Locked task','other','review',2,'sha-2',1,1);",
    )
    .unwrap();

    // The readable task's mail with private mail interleaved between every
    // pair, so each readable row's table-global position has a hidden row
    // immediately in front of it.
    let message = |id: &str, alias: &str, body: &str, task: &str| {
        conn.execute(
            "INSERT INTO messages(id,alias,body,sender,source,state,created,completed,task_id)
             VALUES (?1,?2,?3,'operator','send','completed',?4,?4,?5)",
            rusqlite::params![id, alias, body, CREATED, task],
        )
        .unwrap();
    };
    message("m1", "reader", "opened-1", "open-task");
    message("h1", "other", "hidden-1", "locked-task");
    message("m2", "reader", "opened-22", "open-task");
    message("h2", "other", "hidden-2", "locked-task");
    message("m3", "reader", "opened-333", "open-task");

    // The same interleaving for verdicts: the readable history's rows are
    // separated by private verdicts in the shared, append-only table.
    let verdict = |task: &str, revision: i64, sha: &str, verdict: &str| {
        conn.execute(
            "INSERT INTO verdicts(task_id,revision,sha,verdict,reviewer,evidence,message,verify,created)
             VALUES (?1,?2,?3,?4,'reviewer',?5,?6,?7,?8)",
            rusqlite::params![
                task,
                revision,
                sha,
                verdict,
                json!({"suite": "acceptance", "passed": verdict == "pass"}).to_string(),
                format!("{task} revision {revision} {verdict}"),
                json!({"checked": true, "skipped": false}).to_string(),
                CREATED,
            ],
        )
        .unwrap();
    };
    verdict("open-task", 1, "sha-1", "pass");
    verdict("locked-task", 1, "sha-1", "fail");
    verdict("open-task", 2, "sha-2", "pass");
    verdict("locked-task", 2, "sha-2", "fail");
    (temp, shared)
}

fn rows<'a>(task: &'a Value, field: &str) -> &'a Vec<Value> {
    task[field]
        .as_array()
        .unwrap_or_else(|| panic!("task detail field '{field}': {task}"))
}

fn ids(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|row| row["id"].as_str().expect("row identity").to_string())
        .collect()
}

/// The operator's numeric positions for `rows`; a missing one fails here.
fn positions(rows: &[Value], what: &str) -> Vec<i64> {
    rows.iter()
        .map(|row| {
            row["seq"]
                .as_i64()
                .unwrap_or_else(|| panic!("the operator lost the numeric {what} sequence: {row}"))
        })
        .collect()
}

fn without_seq(value: &Value) -> Value {
    let mut value = value.clone();
    assert!(value.is_object(), "row is not an object: {value}");
    value
        .as_object_mut()
        .expect("object checked above")
        .remove("seq");
    value
}

#[test]
fn scoped_task_detail_hides_the_global_message_and_verdict_sequence() {
    let (_temp, shared) = detail_fixture();
    let pid = std::process::id();
    let reader = || Asserted::Agent("reader".to_string());

    // The seam must drive the real caller derivation, not an ambient identity.
    scoped(reader(), || {
        let caller = shared.agent_caller(pid, "acceptance reader").unwrap();
        assert!(matches!(&caller, AgentCaller::Agent(alias) if alias == "reader"));
    });
    scoped(Asserted::Operator, || {
        let caller = shared.agent_caller(pid, "acceptance operator").unwrap();
        assert!(matches!(caller, AgentCaller::Operator));
    });

    let show = |who: Asserted, task: &str| {
        scoped(who, || {
            shared.dispatch("task_show", &json!({"task": task}), pid)
        })
    };
    let scoped_detail = show(reader(), "open-task").expect("reader reads its readable task");
    let operator_detail = show(Asserted::Operator, "open-task").expect("operator reads the task");
    let hidden_detail =
        show(Asserted::Operator, "locked-task").expect("operator reads the private task");

    let scoped_task = &scoped_detail["task"];
    let operator_task = &operator_detail["task"];

    // Message rows: the authorized rows only, in order, with their content.
    let scoped_messages = rows(scoped_task, "messages");
    let operator_messages = rows(operator_task, "messages");
    assert_eq!(ids(scoped_messages), vec!["m1", "m2", "m3"]);
    assert_eq!(ids(operator_messages), vec!["m1", "m2", "m3"]);
    assert_eq!(
        scoped_messages
            .iter()
            .map(|row| row["body"].as_str().expect("message body"))
            .collect::<Vec<_>>(),
        vec!["opened-1", "opened-22", "opened-333"]
    );
    for row in scoped_messages {
        assert!(
            row.get("seq").is_none(),
            "scoped message row exposed the table-global sequence: {row}"
        );
    }
    // The scoped projection drops only what it always dropped, plus the
    // table-global position; no authorized message content is lost.
    let scoped_equivalent = operator_messages
        .iter()
        .map(|row| {
            let mut row = without_seq(row);
            let fields = row.as_object_mut().expect("message row");
            for field in ["reply_to", "turn_id", "worktree"] {
                fields.remove(field);
            }
            row
        })
        .collect::<Vec<_>>();
    assert_eq!(scoped_messages, &scoped_equivalent);

    // Verdict history: same rows and content, no global position.
    let scoped_verdicts = rows(scoped_task, "verdicts");
    let operator_verdicts = rows(operator_task, "verdicts");
    assert_eq!(scoped_verdicts.len(), 2);
    assert_eq!(
        scoped_verdicts
            .iter()
            .map(|row| row["revision"].clone())
            .collect::<Vec<_>>(),
        vec![json!(1), json!(2)]
    );
    for row in scoped_verdicts {
        assert!(
            row.get("seq").is_none(),
            "scoped verdict exposed the table-global sequence: {row}"
        );
    }
    assert_eq!(
        scoped_verdicts,
        &operator_verdicts
            .iter()
            .map(without_seq)
            .collect::<Vec<_>>()
    );

    // The current verdict keeps every authorized field but the position.
    let scoped_latest = &scoped_task["latest_verdict"];
    assert!(
        scoped_latest.is_object(),
        "the reader lost the current verdict: {scoped_task}"
    );
    assert!(
        scoped_latest.get("seq").is_none(),
        "the current verdict exposed the table-global sequence: {scoped_latest}"
    );
    assert_eq!(scoped_latest["revision"], json!(2));
    assert_eq!(scoped_latest["verdict"], json!("pass"));
    assert_eq!(scoped_latest["sha"], json!("sha-2"));
    assert_eq!(
        scoped_latest,
        &without_seq(&operator_task["latest_verdict"])
    );

    // The operator keeps the numeric management view: the readable
    // positions move by the rows the reader cannot see.
    let operator_message_seq = positions(operator_messages, "message");
    assert!(
        operator_message_seq
            .windows(2)
            .all(|pair| pair[1] > pair[0]),
        "operator message positions are not ordered: {operator_message_seq:?}"
    );
    assert!(
        operator_message_seq
            .windows(2)
            .all(|pair| pair[1] - pair[0] == 2),
        "hidden traffic did not interleave the readable messages: {operator_message_seq:?}"
    );
    let hidden_messages = rows(&hidden_detail["task"], "messages");
    assert_eq!(ids(hidden_messages), vec!["h1", "h2"]);
    let hidden_message_seq = positions(hidden_messages, "message");
    assert!(operator_message_seq[0] < hidden_message_seq[0]);
    assert!(hidden_message_seq[0] < operator_message_seq[1]);
    assert!(operator_message_seq[1] < hidden_message_seq[1]);
    assert!(hidden_message_seq[1] < operator_message_seq[2]);

    let operator_verdict_seq = positions(operator_verdicts, "verdict");
    assert_eq!(operator_verdict_seq.len(), 2);
    assert_eq!(
        operator_verdict_seq[1] - operator_verdict_seq[0],
        2,
        "hidden verdicts did not interleave the readable history: {operator_verdict_seq:?}"
    );
    assert_eq!(
        operator_task["latest_verdict"]["seq"],
        json!(operator_verdict_seq[1])
    );
    let hidden_verdicts = rows(&hidden_detail["task"], "verdicts");
    assert_eq!(hidden_verdicts.len(), 2);
    let hidden_verdict_seq = positions(hidden_verdicts, "verdict");
    assert!(operator_verdict_seq[0] < hidden_verdict_seq[0]);
    assert!(hidden_verdict_seq[0] < operator_verdict_seq[1]);
    assert!(operator_verdict_seq[1] < hidden_verdict_seq[1]);

    // The private task is not readable at all, so the rows that moved the
    // visible positions were never offered to the reader in the first place.
    let refused = show(reader(), "locked-task");
    assert!(
        refused.is_err(),
        "the reader read the private task's detail: {refused:?}"
    );
}
