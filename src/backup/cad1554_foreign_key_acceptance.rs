//! CAD-1554 independent acceptance: portable coordination export must project
//! a valid human-room store without failing a foreign key that the finished
//! snapshot satisfies, and must still refuse a projection whose finished
//! snapshot does not. Author: independent acceptance (not the implementer).
//! Written from the export entrypoint and the live store schema. Do not
//! weaken it.
//!
//! The CLI's clean-tracker export is `export_portable_selected` with no apps.
//! `inspect_portable` is the native package check. Both the clean export and
//! `--snapshot-working-tree` run the same projection. Fixtures are current
//! `Store::open` databases under a fresh temp directory, with a clean Git
//! tracker. The source is only read by the export.
//!
//! Every foreign key in that schema is `ON DELETE NO ACTION`. Nothing cascades
//! and nothing is `RESTRICT`: `human_rooms` references `topics` and
//! `human_principals`, and the member, read, notification and push rows
//! reference those parents. The positive store is foreign-key clean before
//! projection. It holds that human-room graph beside an ordinary project
//! topic and post, with actor and monitor dispatch flags enabled. Export has
//! to publish a package whose private human tables and room transcript are
//! empty, whose project discussion remains, whose `foreign_key_check` is
//! empty, and whose imported flags are off. Native inspection must accept
//! that package. The quiesced source file digest and schema version stay as
//! seeded. That package is unreachable while each projection statement checks
//! `NO ACTION` on its own: the room row still references the room topic when
//! that topic is deleted, and principal children are still present when the
//! principal table is cleared in catalog order.
//!
//! The negative store is also foreign-key clean before projection, and it
//! does not use that room-topic edge. No coordination column in the real
//! schema has a foreign key to an omitted parent, and every real child of an
//! omitted parent is itself omitted or scrubbed with the room topic, so a
//! valid current store cannot finish projection in an invalid state. The
//! fixture therefore keeps the catalog columns of `topic_posts` and adds one
//! `NO ACTION` foreign key on the existing `sender_uid` column to
//! `human_principals`. A project post with that uid set is retained while the
//! principal table is omitted, so the projection's own foreign-key check
//! refuses. A twin store whose `sender_uid` is unset exports, which shows the
//! refusal is that finished foreign key and not the catalog or the secret
//! scan. No package is published and the source is unchanged.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::backup::portable::{
    export_portable_selected, inspect_portable, PortableExportOptions, Scope,
};
use crate::rollout::db_file;
use crate::store::Store;

const PRINCIPAL: &str = "human-cad1554";
const ROOM: &str = "room-cad1554";
const ROOM_TOPIC: &str = "topic-human-cad1554";
const PROJECT_TOPIC: &str = "topic-project-cad1554";
const PRIVATE_POST: &str = "post-private-cad1554";
const PROJECT_POST: &str = "post-project-cad1554";
const PRIVATE_BODY: &str = "private human transcript cad1554";
const PROJECT_BODY: &str = "ordinary project discussion cad1554";
const ROOM_TITLE: &str = "Synthetic private room";
const HUMAN_SUB: &str = "human-subscriber-cad1554";
const PROJECT_SUB: &str = "project-subscriber-cad1554";
const ROOM_ALIAS: &str = "room-alias-cad1554";
const WORKER: &str = "cad1554-worker";
const MONITOR: &str = "mon-cad1554";

/// Identity of a quiesced source database. The file bytes already contain the
/// schema and every row, so this proof does not hash those again.
struct Proof {
    file_sha256: String,
    version: i64,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "cad1554-acceptance")
        .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
        .env("GIT_COMMITTER_NAME", "cad1554-acceptance")
        .env("GIT_COMMITTER_EMAIL", "acceptance@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn make_tracker(root: &Path) -> PathBuf {
    let tracker = root.join("tracker");
    fs::create_dir_all(tracker.join("tickets")).unwrap();
    git(&tracker, &["init", "-q"]);
    fs::write(
        tracker.join("tickets/CAD-1554.md"),
        "# CAD-1554 synthetic ticket fixture\n",
    )
    .unwrap();
    git(&tracker, &["add", "-A"]);
    git(&tracker, &["commit", "-q", "-m", "CAD-1554 fixture"]);
    tracker
}

fn make_state(root: &Path) -> PathBuf {
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    drop(Store::open(&db_file(&state)).unwrap());
    state
}

fn opts(tracker: &Path) -> PortableExportOptions {
    PortableExportOptions {
        scope: Scope::Coordination,
        tracker: tracker.to_path_buf(),
    }
}

fn open_ro(db: &Path) -> Connection {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn sha_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn quiesce(db: &Path) {
    Connection::open(db)
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
}

fn proof_of(db: &Path) -> Proof {
    let version: i64 = open_ro(db)
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    Proof {
        file_sha256: sha_hex(&fs::read(db).unwrap()),
        version,
    }
}

fn sealed_proof(state: &Path) -> Proof {
    let db = db_file(state);
    quiesce(&db);
    proof_of(&db)
}

fn assert_unchanged(state: &Path, proof: &Proof) {
    let now = proof_of(&db_file(state));
    assert_eq!(now.file_sha256, proof.file_sha256, "source file digest");
    assert_eq!(now.version, proof.version, "source schema version");
}

fn fk_violations(db: &Path) -> usize {
    open_ro(db)
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |_| Ok(()))
        .unwrap()
        .count()
}

fn count(db: &Path, table: &str) -> i64 {
    open_ro(db)
        .query_row(
            &format!("SELECT count(*) FROM {}", quote_ident(table)),
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn human_tables(db: &Path) -> Vec<String> {
    open_ro(db)
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'human\\_%' ESCAPE '\\' ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn no_partial(out: &Path) -> bool {
    let parent = out.parent().unwrap();
    let name = out.file_name().unwrap().to_string_lossy().into_owned();
    let prefix = format!(".{name}.partial-");
    fs::read_dir(parent)
        .unwrap()
        .flatten()
        .all(|entry| !entry.file_name().to_string_lossy().starts_with(&prefix))
}

fn flags(db: &Path) -> (i64, i64, i64) {
    let conn = open_ro(db);
    let enabled: i64 = conn
        .query_row(
            "SELECT enabled FROM agents WHERE alias=?1",
            [WORKER],
            |row| row.get(0),
        )
        .unwrap();
    let (dispatch, auto): (i64, i64) = conn
        .query_row(
            "SELECT dispatch_enabled, auto_dispatch_enabled FROM monitors WHERE id=?1",
            [MONITOR],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    (enabled, dispatch, auto)
}

fn seed_authority(conn: &Connection) {
    conn.execute(
        "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,enabled,created,updated)
         VALUES(?1,'codex','stdio','worker','/tmp/cad1554-synthetic','workspace-write','idle',1,1,1)",
        [WORKER],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO monitors(id,project,owner,interval_secs,state,event_cursor,delivery_configured,delivery_state,dispatch_enabled,auto_dispatch_enabled,created,updated)
         VALUES(?1,'synthetic-project',?2,60,'idle',0,0,'unconfigured',1,1,1,1)",
        params![MONITOR, WORKER],
    )
    .unwrap();
}

fn seed_human_room_and_project(state: &Path) {
    let conn = Connection::open(db_file(state)).unwrap();
    seed_authority(&conn);
    conn.execute(
        "INSERT INTO topics(id,type,source,created) VALUES(?1,'human_room',?2,1)",
        params![ROOM_TOPIC, ROOM],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO topics(id,type,source,created) VALUES(?1,'project','project-cad1554',1)",
        [PROJECT_TOPIC],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO human_principals(id,org,issuer,subject,name,seen) VALUES(?1,'synthetic-org','synthetic-issuer','synthetic-subject','Synthetic person',1)",
        [PRINCIPAL],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO human_rooms(id,topic_id,org,kind,pair,title,creator,created) VALUES(?1,?2,'synthetic-org','human_group',NULL,?3,?4,1)",
        params![ROOM, ROOM_TOPIC, ROOM_TITLE, PRINCIPAL],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO human_room_members(room,kind,member,cutoff,added) VALUES(?1,'human',?2,0,1)",
        params![ROOM, PRINCIPAL],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO human_read_markers(room,human,seq) VALUES(?1,?2,1)",
        params![ROOM, PRINCIPAL],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created) VALUES(?1,?2,NULL,'synthetic-human',?3,1)",
        params![PRIVATE_POST, ROOM_TOPIC, PRIVATE_BODY],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created) VALUES(?1,?2,NULL,'synthetic-author',?3,1)",
        params![PROJECT_POST, PROJECT_TOPIC, PROJECT_BODY],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO topic_subscriptions(topic_id,uid,added) VALUES(?1,?2,1)",
        params![ROOM_TOPIC, HUMAN_SUB],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO topic_subscriptions(topic_id,uid,added) VALUES(?1,?2,1)",
        params![PROJECT_TOPIC, PROJECT_SUB],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO topic_deliveries(post_id,uid,alias,state) VALUES(?1,?2,?3,'pending')",
        params![PRIVATE_POST, HUMAN_SUB, ROOM_ALIAS],
    )
    .unwrap();
}

fn text_count(db: &Path, sql: &str, param: &str) -> i64 {
    open_ro(db)
        .query_row(sql, [param], |row| row.get(0))
        .unwrap()
}

fn assert_project_survives(db: &Path) {
    assert_eq!(
        text_count(
            db,
            "SELECT count(*) FROM topics WHERE id=?1 AND type='project'",
            PROJECT_TOPIC,
        ),
        1,
        "project topic survives"
    );
    let body: String = open_ro(db)
        .query_row(
            "SELECT body FROM topic_posts WHERE id=?1",
            [PROJECT_POST],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(body, PROJECT_BODY, "project discussion survives");
    assert_eq!(
        text_count(
            db,
            "SELECT count(*) FROM topic_subscriptions WHERE topic_id=?1",
            PROJECT_TOPIC,
        ),
        1,
        "project subscription survives"
    );
}

fn assert_room_transcript_absent(db: &Path) {
    assert_eq!(
        open_ro(db)
            .query_row(
                "SELECT count(*) FROM topics WHERE type='human_room'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "room topics are omitted"
    );
    for (label, sql, param) in [
        (
            "room topic",
            "SELECT count(*) FROM topics WHERE id=?1",
            ROOM_TOPIC,
        ),
        (
            "private post",
            "SELECT count(*) FROM topic_posts WHERE id=?1",
            PRIVATE_POST,
        ),
        (
            "private body",
            "SELECT count(*) FROM topic_posts WHERE body=?1",
            PRIVATE_BODY,
        ),
        (
            "room subscription",
            "SELECT count(*) FROM topic_subscriptions WHERE topic_id=?1",
            ROOM_TOPIC,
        ),
        (
            "room subscriber",
            "SELECT count(*) FROM topic_subscriptions WHERE uid=?1",
            HUMAN_SUB,
        ),
        (
            "room delivery",
            "SELECT count(*) FROM topic_deliveries WHERE post_id=?1",
            PRIVATE_POST,
        ),
        (
            "room delivery alias",
            "SELECT count(*) FROM topic_deliveries WHERE alias=?1",
            ROOM_ALIAS,
        ),
    ] {
        assert_eq!(text_count(db, sql, param), 0, "{label} is absent");
    }
    assert_eq!(count(db, "topic_deliveries"), 0, "no room delivery remains");
}

fn assert_human_tables_empty(db: &Path) {
    for table in human_tables(db) {
        assert_eq!(count(db, &table), 0, "{table} leaked into the package");
    }
}

fn assert_tracker_unchanged(tracker: &Path, head: &str) {
    assert_eq!(
        git(
            tracker,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        ""
    );
    assert_eq!(git(tracker, &["rev-parse", "HEAD"]), head);
}

/// `topic_posts.sender_uid` stays a catalog column and gains one `NO ACTION`
/// foreign key to the omitted `human_principals` parent, the same action as
/// every constraint in the live schema. Column names are unchanged, so the
/// closed catalog still admits the table. The source is valid: the principal
/// row exists until projection deletes it.
fn declare_sender_uid_principal_fk(db: &Path) {
    let mut conn = Connection::open(db).unwrap();
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute_batch(
        "CREATE TABLE topic_posts_cad1554(
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT UNIQUE NOT NULL,
            topic_id TEXT NOT NULL REFERENCES topics(id),
            sender_uid TEXT REFERENCES human_principals(id),
            sender TEXT NOT NULL,
            body TEXT NOT NULL,
            created REAL NOT NULL,
            request_message TEXT);
         INSERT INTO topic_posts_cad1554(
            seq, id, topic_id, sender_uid, sender, body, created, request_message)
         SELECT seq, id, topic_id, sender_uid, sender, body, created, request_message
         FROM topic_posts;
         DROP TABLE topic_posts;
         ALTER TABLE topic_posts_cad1554 RENAME TO topic_posts;
         CREATE INDEX topic_posts_topic ON topic_posts(topic_id, seq);",
    )
    .unwrap();
    tx.commit().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
}

fn sender_uid_targets_principal(db: &Path) -> bool {
    let conn = open_ro(db);
    let mut stmt = conn
        .prepare("PRAGMA foreign_key_list(topic_posts)")
        .unwrap();
    let mut rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>("from")?,
                row.get::<_, String>("table")?,
            ))
        })
        .unwrap();
    rows.any(|row| {
        let (from, table) = row.unwrap();
        from == "sender_uid" && table == "human_principals"
    })
}

fn seed_project_post_and_principal(state: &Path, sender_uid: Option<&str>) {
    let db = db_file(state);
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO human_principals(id,org,issuer,subject,name,seen) VALUES(?1,'synthetic-org','synthetic-issuer','synthetic-subject','Synthetic person',1)",
            [PRINCIPAL],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO topics(id,type,source,created) VALUES(?1,'project','project-cad1554',1)",
            [PROJECT_TOPIC],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created) VALUES(?1,?2,NULL,'synthetic-author',?3,1)",
            params![PROJECT_POST, PROJECT_TOPIC, PROJECT_BODY],
        )
        .unwrap();
    }
    declare_sender_uid_principal_fk(&db);
    if let Some(uid) = sender_uid {
        Connection::open(&db)
            .unwrap()
            .execute(
                "UPDATE topic_posts SET sender_uid=?1 WHERE id=?2",
                params![uid, PROJECT_POST],
            )
            .unwrap();
    }
    assert!(
        sender_uid_targets_principal(&db),
        "fixture foreign key on sender_uid is present"
    );
    assert_eq!(fk_violations(&db), 0, "source fixture is foreign-key clean");
}

#[test]
fn cad1554_export_omits_human_room_graph_and_keeps_project_discussion() {
    let owned = tempfile::Builder::new()
        .prefix("cad1554-fk-")
        .tempdir()
        .unwrap();
    let root = owned.path().join("project-and-room");
    fs::create_dir_all(&root).unwrap();
    let tracker = make_tracker(&root);
    let head = git(&tracker, &["rev-parse", "HEAD"]);
    let state = make_state(&root);
    seed_human_room_and_project(&state);
    let source = db_file(&state);
    assert_eq!(
        open_ro(&source)
            .query_row("SELECT version FROM schema_version", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        crate::rollout::SCHEMA_VERSION,
        "fixture uses the current store schema"
    );
    assert_eq!(
        fk_violations(&source),
        0,
        "source foreign_key_check is empty"
    );
    assert_eq!(count(&source, "human_principals"), 1);
    assert_eq!(count(&source, "human_rooms"), 1);
    assert_eq!(count(&source, "human_room_members"), 1);
    assert_eq!(count(&source, "human_read_markers"), 1);
    assert_eq!(flags(&source), (1, 1, 1));
    let proof = sealed_proof(&state);

    let package = root.join("package");
    let report = export_portable_selected(&state, &package, &opts(&tracker), &[])
        .expect("valid human-room store exports a coordination package");
    assert_eq!(report["full_workspace"], false);
    assert_eq!(report["schema_version"], proof.version);
    assert!(package.is_dir(), "export publishes the package directory");
    assert!(no_partial(&package), "no partial staging directory remains");
    let inspected = inspect_portable(&package).expect("native inspect admits the package");
    assert_eq!(inspected["status"], "verified_coordination_package");
    assert_eq!(inspected["import_ready"], false);
    assert_eq!(inspected["full_workspace"], false);
    assert_eq!(
        inspected["verification"]["database_integrity_catalog_authority"],
        true
    );
    assert_eq!(inspected["schema_version"], proof.version);
    for table in [
        "human_principals",
        "human_rooms",
        "human_room_members",
        "human_read_markers",
    ] {
        assert!(
            omitted_count(&report, table) >= 1 && omitted_count(&inspected, table) >= 1,
            "{table} is documented as omitted"
        );
    }

    let exported = package.join("cadence.sqlite3");
    assert_eq!(
        fk_violations(&exported),
        0,
        "exported database foreign_key_check is empty"
    );
    assert_eq!(
        proof_of(&exported).version,
        proof.version,
        "package keeps the source schema version"
    );
    assert_human_tables_empty(&exported);
    assert_room_transcript_absent(&exported);
    assert_project_survives(&exported);
    assert_eq!(
        flags(&exported),
        (0, 0, 0),
        "imported actor and dispatch flags are disabled"
    );
    let packaged = fs::read(&exported).unwrap();
    for marker in [
        PRIVATE_BODY,
        ROOM_TITLE,
        ROOM,
        ROOM_TOPIC,
        PRIVATE_POST,
        ROOM_ALIAS,
        HUMAN_SUB,
        PRINCIPAL,
    ] {
        assert!(
            !packaged
                .windows(marker.len())
                .any(|window| window == marker.as_bytes()),
            "package database still contains {marker}"
        );
    }
    assert!(
        packaged
            .windows(PROJECT_BODY.len())
            .any(|window| window == PROJECT_BODY.as_bytes()),
        "project discussion is present in the package database"
    );

    assert_eq!(flags(&source), (1, 1, 1), "source flags stay enabled");
    assert_eq!(count(&source, "human_rooms"), 1, "source room row remains");
    assert_eq!(
        text_count(
            &source,
            "SELECT count(*) FROM topic_posts WHERE body=?1",
            PRIVATE_BODY,
        ),
        1,
        "source private transcript remains"
    );
    assert_unchanged(&state, &proof);
    assert_tracker_unchanged(&tracker, &head);
}

#[test]
fn cad1554_export_refuses_retained_reference_to_omitted_principal() {
    let owned = tempfile::Builder::new()
        .prefix("cad1554-fk-")
        .tempdir()
        .unwrap();

    let control_root = owned.path().join("unset-sender");
    fs::create_dir_all(&control_root).unwrap();
    let control_tracker = make_tracker(&control_root);
    let control_state = make_state(&control_root);
    seed_project_post_and_principal(&control_state, None);
    let control_proof = sealed_proof(&control_state);
    let control_package = control_root.join("package");
    export_portable_selected(
        &control_state,
        &control_package,
        &opts(&control_tracker),
        &[],
    )
    .expect(
        "the same catalog with sender_uid unset exports; a refusal here is not the foreign-key guard",
    );
    let control_db = control_package.join("cadence.sqlite3");
    assert_eq!(fk_violations(&control_db), 0);
    assert_eq!(
        count(&control_db, "human_principals"),
        0,
        "control projection omits the principal parent"
    );
    let survived: String = open_ro(&control_db)
        .query_row(
            "SELECT body FROM topic_posts WHERE id=?1",
            [PROJECT_POST],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(survived, PROJECT_BODY, "control keeps the project post");
    assert_unchanged(&control_state, &control_proof);

    let root = owned.path().join("retained-sender");
    fs::create_dir_all(&root).unwrap();
    let tracker = make_tracker(&root);
    let head = git(&tracker, &["rev-parse", "HEAD"]);
    let state = make_state(&root);
    seed_project_post_and_principal(&state, Some(PRINCIPAL));
    let source = db_file(&state);
    assert_eq!(linked_sender(&source), PRINCIPAL);
    let proof = sealed_proof(&state);

    let package = root.join("package");
    let err = export_portable_selected(&state, &package, &opts(&tracker), &[]).expect_err(
        "a retained project post that references the omitted principal is a finished foreign-key violation",
    );
    assert!(
        err.to_string().contains("FOREIGN KEY constraint failed"),
        "refusal must be SQLite foreign-key enforcement, not the catalog or secret scan: {err}"
    );
    assert!(
        !package.exists() && no_partial(&package),
        "refused export publishes no package"
    );
    assert_eq!(
        count(&source, "human_principals"),
        1,
        "source principal remains"
    );
    assert_eq!(linked_sender(&source), PRINCIPAL);
    assert_unchanged(&state, &proof);
    assert_tracker_unchanged(&tracker, &head);
}

fn omitted_count(report: &Value, table: &str) -> i64 {
    report["omitted_rows"]
        .as_array()
        .and_then(|rows| {
            rows.iter().find_map(|row| {
                (row[0].as_str() == Some(table)).then_some(row[1].as_i64().unwrap_or(0))
            })
        })
        .unwrap_or(0)
}

fn linked_sender(db: &Path) -> String {
    open_ro(db)
        .query_row(
            "SELECT sender_uid FROM topic_posts WHERE id=?1",
            [PROJECT_POST],
            |row| row.get(0),
        )
        .unwrap()
}
