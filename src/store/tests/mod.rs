use super::*;
use super::{
    agents::*, delivery::*, events::*, kickoff::*, messages::*, plans::*, quota::*, schema::*,
};

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::Result;
use crate::proto::identifier;

use tempfile::TempDir;

const SHA40_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

const SHA40_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn verdict(seq: i64, revision: i64) -> Verdict {
    Verdict {
        seq,
        task_id: "t1".to_string(),
        revision,
        sha: SHA40_A.to_string(),
        verdict: "pass".to_string(),
        reviewer: "rev".to_string(),
        evidence: None,
        message: None,
        verify: None,
        created: 0.0,
    }
}

fn store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(&dir.path().join("t.sqlite3")).unwrap();
    std::fs::create_dir(dir.path().join("w")).unwrap();
    (dir, store)
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

fn approval<'a>(id: &'a str, source: &'a str, head: &'a str, pr: u64) -> NewApproval<'a> {
    NewApproval {
        id: Some(id),
        source,
        action: "merge",
        head_sha: head,
        repo: "favcrm/cadence",
        pr,
    }
}

/// `(kind, approval_id)` rows on the approval stream, in order.
fn approval_rows(s: &Store) -> Vec<(String, String)> {
    let conn = s.conn();
    let mut stmt = conn
        .prepare("SELECT kind, payload FROM events WHERE alias=? ORDER BY seq")
        .unwrap();
    let rows = stmt
        .query_map([APPROVAL_STREAM], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|r| {
            let (kind, raw) = r.unwrap();
            let v: Value = serde_json::from_str(&raw).unwrap();
            (kind, v["approval_id"].as_str().unwrap().to_string())
        })
        .collect();
    rows
}

fn operator_by() -> Value {
    json!({"by": "operator", "by_kind": "operator"})
}

/// A seeded v3 database: one agent, one unattached message, one
/// event — the "copy of the live database" shape A7 names.
fn v3_db(dir: &TempDir) -> std::path::PathBuf {
    let db = dir.path().join("t.sqlite3");
    let cwd = dir.path().join("w");
    std::fs::create_dir(&cwd).unwrap();
    {
        let s = Store::open(&db).unwrap();
        reg(&s, "a1", &cwd);
        s.enqueue("a1", "old work", None, "m1", "user").unwrap();
    }
    // Downgrade to a genuine v3: drop the v4 objects + columns.
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "DROP TABLE verdicts; DROP TABLE tasks; DROP TABLE jobs;
             DROP INDEX msg_task; DROP INDEX events_job;
             ALTER TABLE messages DROP COLUMN task_id;
             ALTER TABLE events DROP COLUMN job_id;
             ALTER TABLE events DROP COLUMN task_id;
             UPDATE schema_version SET version=3;",
    )
    .unwrap();
    drop(conn);
    db
}

/// One helper: a pm + group worker + open job + task, dispatched.
fn seeded_task(s: &Store, cwd: &Path) -> String {
    reg(s, "pm", cwd);
    s.register_agent(&NewAgent {
        alias: "w1",
        provider: "fake",
        endpoint_kind: "fake",
        role: "worker",
        cwd: cwd.to_str().unwrap(),
        sandbox: "read-only",
        instructions: None,
        params: Some(&json!({"upstream": "pm"}).to_string()),
        team_role: None,
        model_policy: None,
    })
    .unwrap();
    s.create_job(
        "j1",
        None,
        "/s.md",
        &"0".repeat(64),
        "pm",
        None,
        None,
        None,
        2,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    s.create_task("j1", "t1", None, Some("w1"), None, None, None, None, None)
        .unwrap();
    let (t, kickoff, dup, _) = s.dispatch_task("t1", None, None, "test").unwrap();
    assert!(!dup && t.state == "dispatched");
    kickoff
}

fn endpoint_at(pid: u32) -> crate::adapter::Identity {
    crate::adapter::Identity {
        thread_id: "t".into(),
        session_id: "s".into(),
        model: None,
        effort: None,
        pid,
        endpoint: None,
        generation: Some("g1".into()),
        attach: None,
    }
}

fn recorded_start(s: &Store, alias: &str) -> (Option<i64>, Option<i64>) {
    let a = s.agent(alias).unwrap();
    (a.pid, a.pid_start)
}

fn run_kickoff(s: &Store, kickoff: &str) -> Message {
    match s.take_queued("w1").unwrap() {
        Take::Message(m) => assert_eq!(m.id, kickoff),
        _ => panic!("expected kickoff"),
    }
    s.mark_running(kickoff, "fake-1-x").unwrap();
    s.message(kickoff).unwrap().unwrap()
}

const CAD162_GEN: &str = "0123456789abcdef0123456789abcdef";

/// `alias` on `(provider, kind)` at `CAD162_GEN`, holding one running
/// message `m-<alias>` under `token`.
fn cad162_turn(s: &Store, cwd: &Path, alias: &str, (provider, kind): (&str, &str), token: &str) {
    s.register_agent(&NewAgent {
        alias,
        provider,
        endpoint_kind: kind,
        role: "worker",
        cwd: cwd.to_str().unwrap(),
        sandbox: "read-only",
        instructions: None,
        params: None,
        team_role: None,
        model_policy: None,
    })
    .unwrap();
    s.set_identity(
        alias,
        &crate::adapter::Identity {
            thread_id: format!("thread-{alias}"),
            session_id: format!("session-{alias}"),
            model: None,
            effort: None,
            pid: 4242,
            endpoint: None,
            generation: Some(CAD162_GEN.to_string()),
            attach: None,
        },
    )
    .unwrap();
    let id = format!("m-{alias}");
    s.enqueue(alias, "work", None, &id, "user").unwrap();
    let Take::Message(m) = s.take_queued(alias).unwrap() else {
        panic!("{alias}: message must be claimed");
    };
    s.mark_running(&m.id, token).unwrap();
}

fn cad162_refusals(s: &Store, alias: &str) -> Vec<String> {
    s.events_tail(alias, 50)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "turn_adopt_refused")
        .map(|e| e.payload["reason"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// A worker on an explicit-reporting pty endpoint (`devin/pty`,
/// `Reporting::Explicit` in `adapter::registry`) so dispatched
/// kickoffs carry the `message result` contract the screen probe
/// tails.
fn reg_pty(s: &Store, alias: &str, cwd: &Path) {
    s.register_agent(&NewAgent {
        alias,
        provider: "devin",
        endpoint_kind: "pty",
        role: "worker",
        cwd: cwd.to_str().unwrap(),
        sandbox: "read-only",
        instructions: None,
        params: Some(&json!({"upstream": "pm"}).to_string()),
        team_role: None,
        model_policy: None,
    })
    .unwrap();
}

/// A pm + open job so `dispatch_task` mints real kickoff ids.
fn seeded_job(s: &Store, cwd: &Path) {
    reg(s, "pm", cwd);
    s.create_job(
        "j1",
        None,
        "/s.md",
        &"0".repeat(64),
        "pm",
        None,
        None,
        None,
        2,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
}

/// Dispatch `task` and return the durable message id plus the
/// enqueued body — the exact text a pty adapter would paste.
fn dispatch_body(s: &Store, task: &str, msg: Option<&str>) -> (String, String) {
    let (_, kickoff, dup, _) = s.dispatch_task(task, None, msg, "test").unwrap();
    assert!(!dup);
    let body = s.message(&kickoff).unwrap().unwrap().body;
    (kickoff, body)
}

/// The slice the pty render probe hashes (private helpers in
/// `src/adapter/pty/mod.rs`: `PROBE_SLICE` scalars off the tail,
/// whitespace stripped). Restated here because the store cannot
/// import them — the assertions below measure real dispatch bodies
/// against that documented window.
fn probe_slice(body: &str) -> String {
    let tail: String = body
        .chars()
        .rev()
        .take(64)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    normalized(&tail)
}

/// `normalize_screen` restated for the count model: the probe counts
/// `slice` occurrences inside the visible pane stripped of whitespace
/// and inline markdown markers.
fn normalized(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !['`', '*', '_', '~'].contains(c))
        .collect()
}

/// The contract restated independently of `kickoff_correlation`:
/// ` Correlation: ` + 32 lowercase hex of SHA-256(message id) + `.`.
fn expected_correlation(message_id: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(message_id.as_bytes()));
    format!(" Correlation: {}.", &digest[..32])
}

fn defaults_body(revision: i64, providers: &str) -> String {
    format!(r#"{{"expected_revision":{revision},"config":{{"schema":1,"providers":{providers}}}}}"#)
}

fn claude_worker<'a>(alias: &'a str, cwd: &'a str, team: Option<&'a str>) -> NewAgent<'a> {
    NewAgent {
        alias,
        provider: "claude",
        endpoint_kind: "managed",
        role: "worker",
        cwd,
        sandbox: "read-only",
        instructions: None,
        params: None,
        team_role: team,
        model_policy: None,
    }
}

fn steer_as_pm(priority: Priority, supersedes: &[String]) -> Steer<'_> {
    Steer {
        priority,
        supersedes,
        by: "pm",
        by_kind: "agent",
    }
}

fn send_steered(
    s: &Store,
    alias: &str,
    reply_to: Option<&str>,
    id: &str,
    steer: &Steer,
) -> Result<(bool, String)> {
    s.enqueue_steered(
        alias,
        "the current instruction",
        reply_to,
        id,
        "user",
        None,
        None,
        None,
        &Sender::Unattributed,
        steer,
        None,
    )
}

fn strings(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| id.to_string()).collect()
}

/// `(id, body)` of every routed notice queued on `alias`.
fn notices(s: &Store, alias: &str) -> Vec<(String, String)> {
    s.messages(alias)
        .unwrap()
        .into_iter()
        .filter(|m| m.source == "worker_notice")
        .map(|m| (m.id, m.body))
        .collect()
}

include!("agents.rs");
include!("delivery.rs");
include!("events.rs");
include!("kickoff.rs");
include!("plans.rs");
include!("queue.rs");
include!("schema.rs");

mod app_runs;

mod app_contexts;

#[test]
fn cad688_schema20_connection_ids_backfill_atomically_and_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cadence.sqlite3");
    let initialized = Store::open_for_schema_tests(&path).unwrap();
    reg(&initialized, "grant-worker", dir.path());
    drop(initialized);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE platform_credentials;
        CREATE TABLE platform_credentials(platform TEXT NOT NULL,account TEXT NOT NULL,scopes TEXT NOT NULL,fingerprint TEXT NOT NULL,custody TEXT NOT NULL,exchange TEXT NOT NULL,enrolled_at REAL NOT NULL,by TEXT NOT NULL,PRIMARY KEY(platform,account));
        INSERT INTO platform_credentials VALUES('fixture','old-account','[\"widgets:read\"]','old-fingerprint','file','token',1,'operator');
        INSERT INTO platform_credentials VALUES('fixture','second-account','[\"widgets:read\"]','second-fingerprint','file','token',2,'operator');
        INSERT INTO platform_grants VALUES('grant-worker','fixture','old-account','[\"widgets:read\"]',1,'operator');
        INSERT INTO platform_defaults VALUES('legacy-project','fixture','old-account',1,'operator');
        INSERT INTO app_runs(id,install_id,epoch,bundle_digest,snapshot,snapshot_digest,owner_pm,request_id,state,created,updated) VALUES('kept-run','kept-install',1,'digest','{}','digest','grant-worker','kept-request','succeeded',1,1);
        INSERT INTO platform_effects(effect_id,request,agent,platform,account,tool,input,input_summary,preview,scopes,state,staged_at,updated_at) VALUES('kept-effect','kept-effect-request','grant-worker','fixture','old-account','widgets.publish','{}','kept input','kept preview','[\"widgets:publish\"]','verified',1,1);
        DROP TABLE IF EXISTS connection_metadata;
        DROP INDEX app_runs_context;
        ALTER TABLE app_runs DROP COLUMN context_id;
        DROP TABLE app_contexts;
        UPDATE schema_version SET version=20;
        CREATE TRIGGER reject_connection_schema BEFORE UPDATE ON schema_version WHEN NEW.version=21 BEGIN SELECT RAISE(ABORT,'migration denied'); END;").unwrap();
    assert!(Store::open_for_schema_tests(&path).is_err());
    assert_eq!(
        db.query_row("SELECT version FROM schema_version", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        20
    );
    assert!(db
        .prepare("SELECT connection_id FROM platform_credentials")
        .is_err());
    for name in ["connection_metadata", "platform_connection_id"] {
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name=?",
                [name],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0,
            "{name} leaked from rollback"
        );
    }
    db.execute_batch("DROP TRIGGER reject_connection_schema")
        .unwrap();
    drop(db);
    let store = Store::open_for_schema_tests(&path).unwrap();
    let row = store
        .platform_credential("fixture", "old-account")
        .unwrap()
        .unwrap();
    assert!(!row.connection_id.is_empty());
    assert_eq!(row.credential_revision, 1);
    assert_eq!(row.fingerprint, "old-fingerprint");
    let second = store
        .platform_credential("fixture", "second-account")
        .unwrap()
        .unwrap();
    assert_ne!(row.connection_id, second.connection_id);
    for table in [
        "platform_grants",
        "platform_defaults",
        "app_runs",
        "platform_effects",
    ] {
        assert_eq!(
            store
                .conn()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1,
            "{table}"
        );
    }
    let namespace = store.connection_workspace_id().unwrap();
    drop(store);
    let reopened = Store::open_for_schema_tests(&path).unwrap();
    assert_eq!(
        reopened
            .platform_credential("fixture", "old-account")
            .unwrap()
            .unwrap()
            .connection_id,
        row.connection_id
    );
    assert_eq!(reopened.connection_workspace_id().unwrap(), namespace);
}

#[test]
fn cad688_store_rotation_cannot_replace_identity_or_reset_revision() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("test.sqlite3")).unwrap();
    let mut record = CredentialRecord {
        connection_id: "conn-first".into(),
        credential_revision: 1,
        platform: "fixture".into(),
        account: "work".into(),
        scopes: vec!["widgets:read".into()],
        fingerprint: "first".into(),
        custody: "file".into(),
        exchange: "token".into(),
        enrolled_at: 1.0,
        by: "operator".into(),
    };
    let mut invalid = record.clone();
    invalid.connection_id.clear();
    assert!(store.platform_enroll(&invalid, false, None).is_err());
    invalid = record.clone();
    invalid.credential_revision = 0;
    assert!(store.platform_enroll(&invalid, false, None).is_err());
    store.platform_enroll(&record, false, None).unwrap();
    let before = store
        .conn()
        .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
        .unwrap();
    for (id, revision) in [
        ("conn-replacement", 2),
        ("conn-first", 1),
        ("conn-first", 3),
    ] {
        invalid = record.clone();
        invalid.connection_id = id.into();
        invalid.credential_revision = revision;
        invalid.fingerprint = "replacement".into();
        assert!(store.platform_enroll(&invalid, true, None).is_err());
        let retained = store
            .platform_credential("fixture", "work")
            .unwrap()
            .unwrap();
        assert_eq!(retained.connection_id, "conn-first");
        assert_eq!(retained.credential_revision, 1);
        assert_eq!(retained.fingerprint, "first");
        assert_eq!(
            store
                .conn()
                .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            before
        );
    }
    record.credential_revision = 2;
    record.fingerprint = "second".into();
    store.platform_enroll(&record, true, None).unwrap();
    assert_eq!(
        store
            .platform_credential("fixture", "work")
            .unwrap()
            .unwrap()
            .credential_revision,
        2
    );
}

#[test]
fn cad688_duplicate_connection_id_cannot_replace_another_credential() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("test.sqlite3")).unwrap();
    reg(&store, "keeper", dir.path());
    let record = CredentialRecord {
        connection_id: "conn-existing".into(),
        credential_revision: 1,
        platform: "fixture".into(),
        account: "original".into(),
        scopes: vec!["widgets:read".into()],
        fingerprint: "original-fingerprint".into(),
        custody: "file".into(),
        exchange: "token".into(),
        enrolled_at: 1.0,
        by: "operator".into(),
    };
    store.platform_enroll(&record, false, None).unwrap();
    store
        .platform_grant_add("keeper", "fixture", "original", &record.scopes, "operator")
        .unwrap();
    let grants = store.platform_grants(None).unwrap();
    let events = store
        .conn()
        .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
        .unwrap();
    let mut collision = record.clone();
    collision.account = "replacement".into();
    collision.fingerprint = "replacement-fingerprint".into();
    assert!(
        store.platform_enroll(&collision, false, None).is_err(),
        "unique-ID collision replaced another account"
    );
    assert!(store
        .platform_credential("fixture", "replacement")
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .platform_credential("fixture", "original")
            .unwrap()
            .unwrap()
            .to_json(),
        record.to_json()
    );
    assert_eq!(store.platform_grants(None).unwrap(), grants);
    assert_eq!(
        store
            .conn()
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        events
    );
}

mod app_bindings;
mod app_effects;
