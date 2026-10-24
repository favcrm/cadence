use std::path::Path;

use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::*;
use crate::store::NewAgent;

/// Seeded alphanumeric noise — secret fixtures are built at runtime
/// so no literal in this file has a credential shape.
fn noise(seed: &str, n: usize) -> String {
    const ALPHANUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::new();
    let mut counter = 0u32;
    while out.len() < n {
        for b in Sha256::digest(format!("{seed}:{counter}").as_bytes()) {
            if out.len() < n {
                out.push(ALPHANUM[b as usize % ALPHANUM.len()] as char);
            }
        }
        counter += 1;
    }
    out
}

/// A GitHub classic PAT shape: `ghp_` plus 36 characters.
fn github_token(seed: &str) -> String {
    [&["gh", "p_"].concat(), noise(seed, 36).as_str()].concat()
}

fn store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
    (dir, s)
}

fn reg(s: &Store, alias: &str, cwd: &Path) {
    s.register_agent(&NewAgent {
        alias,
        provider: "fake",
        endpoint_kind: "managed",
        role: "worker",
        cwd: cwd.to_str().unwrap(),
        sandbox: "read-only",
        instructions: None,
        params: None,
        team_role: None,
        model_policy: None,
    })
    .unwrap();
}

fn version(db: &Path) -> i64 {
    Connection::open(db)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap()
}

fn has_table(db: &Path, name: &str) -> bool {
    Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        == 1
}

#[test]
fn migration_v12_to_v13_adds_threads_and_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("t.sqlite3");
    {
        let s = Store::open(&db).unwrap();
        reg(&s, "m1", dir.path());
        s.enqueue("m1", "before the migration", None, "x1", "user")
            .unwrap();
    }
    // A genuine v12: no thread objects.
    Connection::open(&db)
        .unwrap()
        .execute_batch(
            "DROP TABLE thread_entries; DROP TABLE threads;
             UPDATE schema_version SET version=12;",
        )
        .unwrap();
    assert!(!has_table(&db, "threads"));
    {
        let s = Store::open_for_schema_tests(&db).unwrap();
        assert_eq!(
            s.message("x1").unwrap().unwrap().body,
            "before the migration"
        );
        s.ensure_thread("m1").unwrap();
        s.enqueue("m1", "after", None, "x2", "user").unwrap();
        assert_eq!(s.thread_entries("m1", 0, 10).unwrap().len(), 1);
    }
    assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
    assert!(has_table(&db, "threads") && has_table(&db, "thread_entries"));
    // Half-applied: objects present, version rolled back — converges
    // and keeps the rows.
    Connection::open(&db)
        .unwrap()
        .execute("UPDATE schema_version SET version=12", [])
        .unwrap();
    {
        let s = Store::open_for_schema_tests(&db).unwrap();
        assert_eq!(s.thread_entries("m1", 0, 10).unwrap().len(), 1);
    }
    assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
    // Reopening a current store is a no-op.
    let s = Store::open(&db).unwrap();
    assert_eq!(s.thread_entries("m1", 0, 10).unwrap().len(), 1);
    assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
}

/// CAD-1098 (I10): v31 -> v32 rebuilds `threads` in one transaction;
/// every existing thread and entry stays exactly where it was (home),
/// the partial unique indexes bind afterwards, and a half-applied or
/// repeated run converges.
#[test]
fn v32_migration_keeps_history_home() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("t.sqlite3");
    let (thread_id, before) = {
        let s = Store::open(&db).unwrap();
        reg(&s, "master", dir.path());
        let thread = s.ensure_thread("master").unwrap();
        s.enqueue("master", "history one", None, "h1", "user")
            .unwrap();
        s.enqueue_sent(
            "master",
            "history two",
            None,
            "h2",
            "operator",
            None,
            &Sender::Operator,
        )
        .unwrap();
        let entries = s.thread_entries("master", 0, 10).unwrap();
        (
            thread.id,
            entries
                .iter()
                .map(|e| (e.seq, e.text.clone()))
                .collect::<Vec<_>>(),
        )
    };
    // A genuine v31: the inline `alias UNIQUE` table, no conversation
    // columns or indexes.
    Connection::open(&db)
        .unwrap()
        .execute_batch(
            "PRAGMA foreign_keys=OFF; DROP INDEX threads_home_alias; DROP INDEX threads_general;
             DROP INDEX threads_subject; DROP INDEX threads_conv;
             CREATE TABLE threads_old(id TEXT PRIMARY KEY, alias TEXT UNIQUE,
                archived_alias TEXT, created REAL NOT NULL, updated REAL NOT NULL);
             INSERT INTO threads_old SELECT id,alias,archived_alias,created,updated FROM threads;
             DROP TABLE threads; ALTER TABLE threads_old RENAME TO threads;
             UPDATE schema_version SET version=31;",
        )
        .unwrap();
    for _ in 0..2 {
        let s = Store::open_for_schema_tests(&db).unwrap();
        let thread = s.thread("master").unwrap().expect("home thread kept");
        assert_eq!(thread.id, thread_id);
        assert!(thread.is_home() && !thread.is_general && !thread.archived);
        let entries = s.thread_entries("master", 0, 10).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.seq, e.text.clone()))
                .collect::<Vec<_>>(),
            before,
            "history moved or rewritten"
        );
        assert!(entries.iter().all(|e| e.thread_id == thread_id));
        assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
        // Half-applied: version rolled back, objects present.
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE schema_version SET version=31", [])
            .unwrap();
    }
    // The partial unique indexes bind: one home, one General, one
    // subject conversation per (alias, install).
    Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE schema_version SET version=?",
            [crate::rollout::SCHEMA_VERSION],
        )
        .unwrap();
    let conn = Connection::open(&db).unwrap();
    let dup = |sql: &str| conn.execute(sql, []).is_err();
    assert!(dup(
        "INSERT INTO threads(id,alias,created,updated) VALUES('x','master',1,1)"
    ));
    let conv =
        "INSERT INTO threads(id,alias,created,updated,install_id,context_id,is_general,subject) \
                VALUES(?,'master',1,1,'i1','c1',?,?)";
    conn.execute(conv, params!["g1", 1, None::<String>])
        .unwrap();
    assert!(conn
        .execute(conv, params!["g2", 1, None::<String>])
        .is_err());
    conn.execute(conv, params!["s1", 0, Some("campaign:a")])
        .unwrap();
    assert!(conn
        .execute(conv, params!["s2", 0, Some("campaign:a")])
        .is_err());
    conn.execute(conv, params!["n1", 0, None::<String>])
        .unwrap();
    conn.execute(conv, params!["n2", 0, None::<String>])
        .unwrap();
    // A subject cannot also be General, and home cannot carry a scope.
    assert!(conn
        .execute(conv, params!["bad", 1, Some("campaign:b")])
        .is_err());
    assert!(dup(
        "INSERT INTO threads(id,alias,created,updated,is_general) VALUES('bad2','w9',1,1,1)"
    ));
}

#[test]
fn enqueue_records_the_sender_only_for_a_threaded_agent() {
    let (dir, s) = store();
    reg(&s, "master", dir.path());
    reg(&s, "w1", dir.path());
    // No thread yet: nothing is recorded, and reads are empty.
    s.enqueue("master", "early", None, "e0", "user").unwrap();
    assert!(s.thread("master").unwrap().is_none());
    assert!(s.thread_entries("master", 0, 10).unwrap().is_empty());

    let thread = s.ensure_thread("master").unwrap();
    assert_eq!(
        s.ensure_thread("master").unwrap().id,
        thread.id,
        "one per alias"
    );
    s.enqueue_sent(
        "master",
        "hello master",
        None,
        "e1",
        "operator",
        None,
        &Sender::Operator,
    )
    .unwrap();
    // A retry of the same envelope is a duplicate — no second entry.
    s.enqueue_sent(
        "master",
        "hello master",
        None,
        "e1",
        "operator",
        None,
        &Sender::Operator,
    )
    .unwrap();
    s.enqueue_sent(
        "master",
        "from a peer",
        None,
        "e2",
        "user",
        None,
        &Sender::Agent("w1".into()),
    )
    .unwrap();
    s.enqueue("master", "internal", None, "e3", "worker_result")
        .unwrap();
    // Other agents stay untouched.
    s.enqueue("w1", "not threaded", None, "e4", "user").unwrap();

    let entries = s.thread_entries("master", 0, 10).unwrap();
    let shape: Vec<(&str, &str, &str, Option<&str>)> = entries
        .iter()
        .map(|e| {
            (
                e.role.as_str(),
                e.kind.as_str(),
                e.text.as_str(),
                e.message_id.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            ("operator", "message", "hello master", Some("e1")),
            ("system", "message", "from a peer", Some("e2")),
            ("system", "message", "internal", Some("e3")),
        ]
    );
    assert_eq!(entries[1].payload.as_ref().unwrap()["from"], "w1");
    assert_eq!(
        entries[2].payload.as_ref().unwrap()["source"],
        "worker_result"
    );
    assert!(s.thread_entries("w1", 0, 10).unwrap().is_empty());
    assert!(s.thread("w1").unwrap().is_none());
}

/// CAD-1578: a verified CLI send to a master with no home thread must
/// create the thread and record the entry, not drop it silently.
#[test]
fn a_cli_send_to_a_threadless_master_creates_home_and_records_the_entry() {
    let (dir, s) = store();
    reg(&s, "master", dir.path());
    let auth = CliAuthority {
        org: "org-a".into(),
        sub: "sub-a".into(),
        actor: "operator".into(),
        handle: "owner".into(),
        credential_id: "cred-a".into(),
    };
    assert!(s.thread("master").unwrap().is_none());
    s.enqueue_sent(
        "master",
        "status please",
        None,
        "cli-1",
        "user",
        None,
        &Sender::Cli(auth),
    )
    .unwrap();
    assert!(s.thread("master").unwrap().is_some());
    let entries = s.thread_entries("master", 0, 10).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message_id.as_deref(), Some("cli-1"));
    assert_eq!(entries[0].text, "status please");
}

#[test]
fn finish_records_the_turn_result_without_the_turn_token() {
    let (dir, s) = store();
    reg(&s, "master", dir.path());
    s.ensure_thread("master").unwrap();
    s.enqueue("master", "do it", None, "m1", "user").unwrap();
    let token = format!("claude-{}-{}", noise("gen", 32), noise("id", 32));
    s.mark_running("m1", &token).unwrap();
    s.thread_append_running("master", ROLE_AGENT, KIND_TOOL_CALL, "Bash: ls", None)
        .unwrap();
    let message = s.message("m1").unwrap().unwrap();
    s.finish(
        &message,
        "completed",
        &json!({"turn_id": token, "status": "completed", "text": "all done",
                "stop_reason": "end_turn", "error": null}),
        None,
    )
    .unwrap();
    let entries = s.thread_entries("master", 0, 10).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[1].kind, "tool_call");
    assert_eq!(
        entries[1].message_id.as_deref(),
        Some("m1"),
        "linked to the running turn"
    );
    let result = &entries[2];
    assert_eq!(
        (result.role.as_str(), result.kind.as_str()),
        ("agent", "turn_result")
    );
    assert_eq!(result.text, "all done");
    assert_eq!(result.message_id.as_deref(), Some("m1"));
    assert_eq!(
        result.payload,
        Some(json!({"status": "completed", "stop_reason": "end_turn"}))
    );
    for e in &entries {
        assert!(!e.to_json().to_string().contains(&token), "{e:?}");
    }
}

/// CAD-415: Codex persists items right behind the `turn/start`
/// reply, before `on_started` marks the message `running`. Text that
/// lands in that window — appended or held — still belongs to the
/// turn; with only `running` counted it was recorded with no message.
#[test]
fn items_before_the_turn_is_marked_running_link_to_the_submitting_message() {
    let (dir, s) = store();
    reg(&s, "master", dir.path());
    s.ensure_thread("master").unwrap();
    s.enqueue("master", "status?", None, "x1", "user").unwrap();
    let crate::store::Take::Message(taken) = s.take_queued("master").unwrap() else {
        panic!("x1 was not taken");
    };
    assert_eq!(taken.id, "x1");
    assert_eq!(s.message("x1").unwrap().unwrap().state, "submitting");
    s.thread_append_running(
        "master",
        ROLE_AGENT,
        KIND_ASSISTANT_TEXT,
        "looking",
        Some(json!({"phase": "commentary"})),
    )
    .unwrap();
    s.thread_hold_running("master", "an aside", json!({"phase": null}))
        .unwrap();
    s.mark_running("x1", "t-1").unwrap();
    let message = s.message("x1").unwrap().unwrap();
    s.finish(
        &message,
        "completed",
        &json!({"turn_id": "t-1", "status": "completed", "text": "MOCK_OK"}),
        None,
    )
    .unwrap();
    let shape: Vec<(String, String, Option<String>)> = s
        .thread_entries("master", 0, 10)
        .unwrap()
        .into_iter()
        .map(|e| (e.kind, e.text, e.message_id))
        .collect();
    let x1 = || Some("x1".to_string());
    assert_eq!(
        shape,
        vec![
            ("message".into(), "status?".into(), x1()),
            ("assistant_text".into(), "looking".into(), x1()),
            ("assistant_text".into(), "an aside".into(), x1()),
            ("turn_result".into(), "MOCK_OK".into(), x1()),
        ]
    );
}

#[test]
fn entries_page_by_after_and_limit() {
    let (dir, s) = store();
    reg(&s, "master", dir.path());
    s.ensure_thread("master").unwrap();
    for i in 0..7 {
        s.thread_append(
            "master",
            NewEntry {
                role: ROLE_AGENT,
                kind: KIND_ASSISTANT_TEXT,
                text: &format!("line {i}"),
                payload: None,
                message_id: None,
            },
        )
        .unwrap()
        .unwrap();
    }
    let first = s.thread_entries("master", 0, 3).unwrap();
    assert_eq!(
        first.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        ["line 0", "line 1", "line 2"]
    );
    let next = s
        .thread_entries("master", first.last().unwrap().seq, 3)
        .unwrap();
    assert_eq!(
        next.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        ["line 3", "line 4", "line 5"]
    );
    let rest = s
        .thread_entries("master", next.last().unwrap().seq, 3)
        .unwrap();
    assert_eq!(rest.len(), 1);
    assert!(s
        .thread_entries("master", rest[0].seq, 3)
        .unwrap()
        .is_empty());
    // Limits clamp to the page bounds.
    assert_eq!(s.thread_entries("master", 0, 0).unwrap().len(), 1);
    assert_eq!(s.thread_entries("master", 0, 10_000).unwrap().len(), 7);
    // Unknown roles and kinds are refused, never stored.
    for (role, kind) in [("boss", KIND_MESSAGE), (ROLE_AGENT, "shout")] {
        assert!(s
            .thread_append(
                "master",
                NewEntry {
                    role,
                    kind,
                    text: "x",
                    payload: None,
                    message_id: None,
                },
            )
            .is_err());
    }
}

/// CAD-304 S4 applied to chats: removing an agent archives its
/// thread (rows kept by thread id, a `system` entry marks it); a new
/// agent under the reused alias starts a fresh thread and inherits
/// nothing. The timer gc path archives the same way.
#[test]
fn a_removed_alias_archives_its_thread_and_a_reuse_starts_fresh() {
    let (dir, s) = store();
    reg(&s, "master", dir.path());
    let old = s.ensure_thread("master").unwrap();
    s.enqueue("master", "remember me", None, "m1", "user")
        .unwrap();
    let m = s.message("m1").unwrap().unwrap();
    s.finish(&m, "completed", &json!({"text": "ok"}), None)
        .unwrap();
    s.set_agent_state("master", "stopped", None).unwrap();
    s.remove_agent(
        "master",
        false,
        &json!({"by": "operator", "by_kind": "operator"}),
    )
    .unwrap();
    assert!(s.thread("master").unwrap().is_none());
    assert!(s.thread_entries("master", 0, 10).unwrap().is_empty());
    // The archived rows stay under the old thread id.
    let (archived_alias, alias): (Option<String>, Option<String>) = s
        .conn()
        .query_row(
            "SELECT archived_alias, alias FROM threads WHERE id=?",
            [&old.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(archived_alias.as_deref(), Some("master"));
    assert_eq!(alias, None);
    let kept: Vec<(String, String)> = s
        .conn()
        .prepare("SELECT role, text FROM thread_entries WHERE thread_id=? ORDER BY seq")
        .unwrap()
        .query_map([&old.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(kept.len(), 3, "{kept:?}");
    assert_eq!(kept[2].0, "system");
    assert!(kept[2].1.contains("removed"), "{kept:?}");
    let left: i64 = s
        .conn()
        .query_row(
            "SELECT count(*) FROM events WHERE alias='master' AND job_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0, "no event is left for a reused alias to inherit");

    // A new agent under the same alias: no thread until one starts,
    // and then a different one with none of the old entries.
    reg(&s, "master", dir.path());
    s.enqueue("master", "unthreaded", None, "m2", "user")
        .unwrap();
    assert!(s.thread("master").unwrap().is_none());
    let fresh = s.ensure_thread("master").unwrap();
    assert_ne!(fresh.id, old.id);
    s.enqueue("master", "hello again", None, "m3", "user")
        .unwrap();
    let entries = s.thread_entries("master", 0, 10).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].text, "hello again");

    // Removing again archives the second thread beside the first.
    for id in ["m2", "m3"] {
        let m = s.message(id).unwrap().unwrap();
        s.finish(&m, "completed", &json!({"text": "ok"}), None)
            .unwrap();
    }
    s.set_agent_state("master", "stopped", None).unwrap();
    s.remove_agent(
        "master",
        false,
        &json!({"by": "operator", "by_kind": "operator"}),
    )
    .unwrap();
    let archived: i64 = s
        .conn()
        .query_row(
            "SELECT count(*) FROM threads WHERE archived_alias='master' AND alias IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(archived, 2);
}

/// A refused chat message starts no thread: the thread is created
/// inside the enqueue transaction, after validation.
#[test]
fn a_refused_operator_chat_message_leaves_no_thread() {
    let (dir, s) = store();
    reg(&s, "master", dir.path());
    let too_long = "x".repeat(48_001);
    for body in ["", too_long.as_str()] {
        assert!(s
            .enqueue_sent(
                "master",
                body,
                None,
                "c1",
                "operator",
                None,
                &Sender::OperatorChat,
            )
            .is_err());
    }
    assert!(s.thread("master").unwrap().is_none());
    assert!(s
        .events("master", 0, 100)
        .unwrap()
        .iter()
        .all(|e| e.kind != "thread_created"));
    s.enqueue_sent(
        "master",
        "ok now",
        None,
        "c2",
        "operator",
        None,
        &Sender::OperatorChat,
    )
    .unwrap();
    let entries = s.thread_entries("master", 0, 10).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].role, "operator");
}

#[test]
fn tool_input_secrets_never_reach_the_summary_or_the_store() {
    let token = github_token("thread-tool");
    let plain = noise("plain-password", 12);
    let cmd = format!("curl -H 'Authorization: token {token}' --password {plain} https://x");
    let summary = tool_summary("Bash", &json!({"command": cmd}));
    assert!(summary.starts_with("Bash: curl"), "{summary}");
    assert!(!summary.contains(&token), "{summary}");
    assert!(
        !summary.contains(&plain),
        "argv rule redacts a secret flag: {summary}"
    );
    assert!(summary.len() <= TOOL_SUMMARY_CAP, "{summary}");

    // A field outside the known ones is named, never valued.
    let other = tool_summary("Mystery", &json!({"b": token, "a": 1}));
    assert_eq!(other, "Mystery: {a, b}");
    assert_eq!(tool_summary("Noop", &Value::Null), "Noop");
    let path = tool_summary("Read", &json!({"file_path": "/repo/src/lib.rs"}));
    assert_eq!(path, "Read: /repo/src/lib.rs");
    // Control characters cannot break the one-line summary.
    assert_eq!(
        tool_summary("Bash", &json!({"command": "echo a\necho b"})),
        "Bash: echo a echo b"
    );

    // Whatever an adapter hands over, the store redacts again: text
    // and every payload string.
    let (dir, s) = store();
    reg(&s, "master", dir.path());
    s.ensure_thread("master").unwrap();
    s.thread_append_running(
        "master",
        ROLE_AGENT,
        KIND_TOOL_CALL,
        &format!("Bash: echo {token}"),
        Some(json!({"tool": "Bash", "nested": [{"raw": token}]})),
    )
    .unwrap();
    s.enqueue("master", &format!("pasted {token}"), None, "m1", "user")
        .unwrap();
    let raw: Vec<String> = s
        .conn()
        .prepare("SELECT text || coalesce(payload,'') FROM thread_entries")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(raw.len(), 2);
    for cell in &raw {
        assert!(!cell.contains(&token), "{cell}");
        assert!(cell.contains("[redacted:"), "{cell}");
    }
}

/// CAD-410: a private key whose END marker never reaches the scan —
/// a head-limited read, or a body cut at the [`TEXT_CAP`] scan limit
/// the way a tool-result summary cuts it — keeps none of its body
/// in a summary or a stored entry.
#[test]
fn a_private_key_cut_before_its_end_never_reaches_the_store() {
    let header = ["-----BEGIN RSA ", "PRIVATE", " KEY-----"].concat();
    let footer = ["-----END RSA ", "PRIVATE", " KEY-----"].concat();
    let body: Vec<String> = (0..TEXT_CAP / 64 + 50)
        .map(|i| noise(&format!("thread-pem:{i}"), 64))
        .collect();
    let whole = format!("{header}\n{}\n{footer}\n", body.join("\n"));
    assert!(whole.len() > TEXT_CAP);
    let mut head = take_bytes(&whole, TEXT_CAP);
    head.truncate(head.rfind(char::is_whitespace).unwrap());
    assert!(!head.contains(&footer), "the END marker is past the cut");
    let leaked = |cell: &str| {
        body.iter()
            .find(|line| cell.contains(line.as_str()))
            .cloned()
    };

    let cleaned = clean_text(&head, TEXT_CAP);
    assert_eq!(cleaned, "[redacted:private-key]");
    let first = &head[..head.find('\n').unwrap() + 3 * 65];
    let summary = tool_summary("Bash", &json!({"command": format!("printf '{first}'")}));
    assert!(summary.starts_with("Bash: printf "), "{summary}");
    assert!(summary.ends_with("[redacted:private-key]"), "{summary}");
    assert_eq!(leaked(&summary), None, "{summary}");

    let (dir, s) = store();
    reg(&s, "master", dir.path());
    s.ensure_thread("master").unwrap();
    s.thread_append_running(
        "master",
        ROLE_AGENT,
        KIND_ASSISTANT_TEXT,
        &head,
        Some(json!({"raw": first})),
    )
    .unwrap();
    s.enqueue("master", first, None, "m1", "user").unwrap();
    let raw: Vec<String> = s
        .conn()
        .prepare("SELECT text || coalesce(payload,'') FROM thread_entries")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(raw.len(), 2);
    for cell in &raw {
        assert_eq!(leaked(cell), None, "key body stored");
        assert!(cell.contains("[redacted:private-key]"), "{cell}");
    }
}

#[test]
fn tool_output_is_a_redacted_one_line_summary() {
    let token = github_token("thread-tool-output");
    let summary = tool_result_summary(&json!(format!("TOKEN={token}\nok\u{1b}[0m done")));
    assert!(summary.starts_with("TOKEN=[redacted:"), "{summary}");
    assert!(!summary.contains(&token), "{summary}");
    assert!(summary.ends_with("ok [0m done"), "one line: {summary}");

    // Blocks: text joined, other types only named.
    let blocks = json!([{"type": "text", "text": "a"}, {"type": "image", "source": {}}]);
    assert_eq!(tool_result_summary(&blocks), "a [image]");
    assert_eq!(tool_result_summary(&Value::Null), "");

    // Long output: bounded with the marker; the tail never survives.
    let long = format!("{}TAIL", "y ".repeat(400));
    let summary = tool_result_summary(&json!(long));
    assert!(summary.len() <= TOOL_SUMMARY_CAP, "{}", summary.len());
    assert!(summary.ends_with("…[truncated]"), "{summary}");
    assert!(!summary.contains("TAIL"), "{summary}");
}

#[test]
fn long_text_is_bounded_with_a_marker() {
    let long = "a".repeat(TEXT_CAP + 500);
    let cleaned = clean_text(&long, TEXT_CAP);
    assert!(cleaned.len() <= TEXT_CAP, "{}", cleaned.len());
    assert!(cleaned.ends_with("…[truncated]"));
}

/// CAD-802: `message_app` returns the delivery hint only while the
/// send-time stamp still matches the live proof. Revised or
/// archived contexts — and unbound messages — yield no hint, so a
/// stale scope can never ride a later turn. Fast and deterministic:
/// no daemon, no provider, no waiting.
#[test]
fn cad802_message_app_hint_reproves_before_delivery() {
    use crate::store::app_contexts::ContextConfig;
    use crate::store::Steer;
    use std::collections::BTreeMap;
    let (dir, s) = store();
    reg(&s, "lead", dir.path());
    let config = ContextConfig::new("Client", BTreeMap::new()).unwrap();
    let created = s.app_context_create("install-1", &config, "req-1").unwrap();
    let id = created["context"]["id"].as_str().unwrap().to_string();
    let (_, proof) = s.app_context_proof("install-1", &id).unwrap();
    let stamp = json!({
        "install_id": "install-1",
        "context_id": id,
        "verified": true,
        "context_revision": proof.revision,
        "context_digest": proof.digest,
    });
    s.enqueue_steered(
        "lead",
        "q",
        None,
        "m-1",
        "user",
        None,
        None,
        None,
        &Sender::OperatorChat,
        &Steer::NONE,
        None,
        Some(&stamp),
        None,
    )
    .unwrap();
    let hint = s.message_app("m-1").unwrap().expect("fresh hint");
    assert_eq!(hint["install_id"], json!("install-1"));
    assert_eq!(hint["context_id"], json!(id));
    assert_eq!(hint["label"], json!("Client"));
    assert_eq!(hint["revision"], json!(proof.revision));
    assert!(hint.get("profile").is_none(), "{hint}");
    assert!(hint.get("digest").is_none(), "{hint}");
    assert!(hint.get("verified").is_none(), "{hint}");
    // Unbound messages carry no hint.
    s.enqueue_steered(
        "lead",
        "q2",
        None,
        "m-2",
        "user",
        None,
        None,
        None,
        &Sender::OperatorChat,
        &Steer::NONE,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(s.message_app("m-2").unwrap(), None);
    assert_eq!(s.message_app("no-such-message").unwrap(), None);
    // A revised context invalidates the old stamp.
    let renamed = ContextConfig::new("Renamed", BTreeMap::new()).unwrap();
    s.app_context_update("install-1", &id, proof.revision, &renamed)
        .unwrap();
    assert_eq!(
        s.message_app("m-1").unwrap(),
        None,
        "revised hint must drop"
    );
    // So does an archived one.
    let (_, proof2) = s.app_context_proof("install-1", &id).unwrap();
    s.app_context_archive("install-1", &id, proof2.revision)
        .unwrap();
    assert_eq!(
        s.message_app("m-1").unwrap(),
        None,
        "archived hint must drop"
    );
}
