//! Offline storage/codec tests. No daemon, HTTP identity or remote custody proof.
use cadence_agent::remote_result_outbox::{
    DestinationPin, ResultCommand, ResultOutbox, MAX_COMMAND_BYTES, MAX_PENDING,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::sync::{Arc, Barrier};

fn wire(id: &str) -> Value {
    json!({
        "version": "hosted-cadence-result.v1", "commandId": id,
        "kind": "agent_result", "assignmentId": "assignment-1",
        "taskId": "task-1", "taskRevision": 1, "turnId": "turn-1",
        "reportedHeadSha": "a".repeat(40), "text": "finished"
    })
}
fn command(id: &str) -> ResultCommand {
    ResultCommand::parse_json(&wire(id).to_string()).unwrap()
}
fn pin(org: &str) -> DestinationPin {
    DestinationPin::new(
        org,
        "https://gateway.example.invalid",
        "subject-1",
        "agent-1",
    )
    .unwrap()
}

#[test]
fn strict_codec_refuses_missing_extra_forged_and_invalid_fields() {
    let mut missing = wire("cmd");
    missing.as_object_mut().unwrap().remove("version");
    assert!(ResultCommand::parse_json(&missing.to_string()).is_err());
    for (field, value) in [
        ("version", json!("hosted-cadence-result.v2")),
        ("kind", json!("heartbeat")),
        ("commandId", json!("../../escape")),
        ("taskId", json!("")),
        ("taskRevision", json!(0)),
        ("taskRevision", json!(-1)),
        ("taskRevision", json!(1.5)),
        ("taskRevision", json!(9_007_199_254_740_992u64)),
        ("taskRevision", json!("1")),
        ("reportedHeadSha", json!("A".repeat(40))),
        ("reportedHeadSha", json!("g".repeat(40))),
        ("reportedHeadSha", json!("a".repeat(39))),
        ("text", json!("")),
        ("actor", json!("operator")),
        ("accessToken", json!("synthetic-never-store")),
    ] {
        let mut value_wire = wire("cmd");
        value_wire[field] = value;
        assert!(
            ResultCommand::parse_json(&value_wire.to_string()).is_err(),
            "{field}"
        );
    }
    assert!(ResultCommand::parse_json("[]").is_err());
    assert!(ResultCommand::parse_json("not JSON").is_err());
    let duplicate = wire("cmd")
        .to_string()
        .replacen('{', "{\"commandId\":\"duplicate\",", 1);
    assert!(ResultCommand::parse_json(&duplicate).is_err());
    let lone_surrogate = wire("cmd").to_string().replace("finished", "\\ud800");
    assert!(ResultCommand::parse_json(&lone_surrogate).is_err());
    let mut sha64 = wire("cmd");
    sha64["reportedHeadSha"] = json!("b".repeat(64));
    assert!(ResultCommand::parse_json(&sha64.to_string()).is_ok());
    for spelling in ["1.0", "1e0"] {
        let decoded = wire("cmd").to_string().replace(
            "\"taskRevision\":1",
            &format!("\"taskRevision\":{spelling}"),
        );
        assert_eq!(ResultCommand::parse_json(&decoded).unwrap(), command("cmd"));
    }
}

#[test]
fn canonical_json_and_utf8_digest_match_the_server_golden() {
    let expected = r#"{"version":"hosted-cadence-result.v1","commandId":"cmd-golden","kind":"agent_result","assignmentId":"assignment-1","taskId":"task-1","taskRevision":1,"turnId":"turn-1","reportedHeadSha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","text":"Done \"quoted\"\npath\\file\t😀"}"#;
    let mut reordered = wire("cmd-golden");
    reordered["text"] = json!("Done \"quoted\"\npath\\file\t😀");
    let parsed = ResultCommand::parse_json(&reordered.to_string()).unwrap();
    assert_eq!(parsed.canonical_json(), expected);
    assert_eq!(
        parsed.digest(),
        "ddc2a7269a5b94cd80c5e59e30068a96645864de94413b8833f0a3f47d4da7b6"
    );
    assert_eq!(
        ResultCommand::parse_json(expected).unwrap().digest(),
        parsed.digest()
    );
}

#[test]
fn payload_limit_counts_canonical_utf8_bytes() {
    let mut too_big = wire("cmd");
    too_big["text"] = json!("😀".repeat(MAX_COMMAND_BYTES / 4));
    assert!(ResultCommand::parse_json(&too_big.to_string()).is_err());
    too_big["text"] = json!("x".repeat(MAX_COMMAND_BYTES));
    assert!(ResultCommand::parse_json(&too_big.to_string()).is_err());
    too_big["text"] = json!("😀".repeat(100));
    assert!(ResultCommand::parse_json(&too_big.to_string()).is_ok());
    let mut boundary = wire("boundary");
    boundary["text"] = json!("");
    let space = MAX_COMMAND_BYTES - boundary.to_string().len();
    boundary["text"] = json!("x".repeat(space));
    assert_eq!(
        ResultCommand::parse_json(&boundary.to_string())
            .unwrap()
            .canonical_json()
            .len(),
        MAX_COMMAND_BYTES
    );
    boundary["text"] = json!("x".repeat(space + 1));
    assert!(ResultCommand::parse_json(&boundary.to_string()).is_err());
}

#[test]
fn structural_destination_refuses_unsafe_or_noncanonical_origins() {
    for origin in [
        "http://gateway.example.invalid",
        "https://user:pass@gateway.example.invalid",
        "https://gateway.example.invalid/",
        "https://gateway.example.invalid/path",
        "https://gateway.example.invalid?q=secret",
        "https://gateway.example.invalid#fragment",
        "https://*.example.invalid",
        "https://GATEWAY.example.invalid",
        "https://gateway.example.invalid:443",
        "https://gateway.example.invalid:0444",
        "https://0x7f000001",
        "https://-bad.example",
        "https://bad-.example",
        "https://[::ffff:127.0.0.1]",
    ] {
        assert!(DestinationPin::new("org", origin, "subject", "agent").is_err());
    }
    assert!(DestinationPin::new("", "https://gateway.example.invalid", "s", "a").is_err());
    assert!(DestinationPin::new("org", "https://gateway.example.invalid:8443", "s", "a").is_ok());
    assert!(DestinationPin::new("org", "https://[::1]:8443", "s", "a").is_ok());
}

#[test]
fn reopening_keeps_original_pending_receipt_bytes_and_destination() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("outbox");
    let original = pin("org-1");
    let first = ResultOutbox::open(&dir)
        .unwrap()
        .enqueue(&original, &command("cmd"))
        .unwrap();
    assert_eq!(first.state(), "local_pending");
    let reopened = ResultOutbox::open(&dir).unwrap();
    assert_eq!(reopened.enqueue(&original, &command("cmd")).unwrap(), first);
    let rows = reopened.pending_for(&original).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].receipt(), &first);
    assert_eq!(
        rows[0].command().canonical_json(),
        command("cmd").canonical_json()
    );
    assert!(reopened.pending_for(&pin("org-2")).unwrap().is_empty());
}

#[test]
fn concurrent_independent_handles_keep_one_original_receipt() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("outbox");
    let initial = ResultOutbox::open(&dir).unwrap();
    let barrier = Arc::new(Barrier::new(12));
    let threads: Vec<_> = (0..12)
        .map(|_| {
            let dir = dir.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let independent = ResultOutbox::open(&dir).unwrap();
                barrier.wait();
                independent
                    .enqueue(&pin("org"), &command("same-command"))
                    .unwrap()
            })
        })
        .collect();
    let receipts: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(receipts.iter().all(|r| r == &receipts[0]));
    assert_eq!(initial.pending_for(&pin("org")).unwrap().len(), 1);
}

#[test]
fn changed_result_identity_or_destination_conflicts_without_replacing_bytes() {
    let root = tempfile::tempdir().unwrap();
    let outbox = ResultOutbox::open(&root.path().join("outbox")).unwrap();
    let original = pin("org-1");
    let first = outbox.enqueue(&original, &command("cmd")).unwrap();
    for (field, value) in [
        ("taskId", json!("another-task")),
        ("taskRevision", json!(2)),
        ("reportedHeadSha", json!("b".repeat(40))),
        ("assignmentId", json!("reopened-dispatch")),
        ("turnId", json!("another-turn")),
        ("text", json!("changed")),
    ] {
        let mut changed = wire("cmd");
        changed[field] = value;
        assert!(outbox
            .enqueue(
                &original,
                &ResultCommand::parse_json(&changed.to_string()).unwrap()
            )
            .is_err());
    }
    for destination in [
        pin("org-2"),
        DestinationPin::new(
            "org-1",
            "https://other.example.invalid",
            "subject-1",
            "agent-1",
        )
        .unwrap(),
        DestinationPin::new(
            "org-1",
            "https://gateway.example.invalid",
            "other-subject",
            "agent-1",
        )
        .unwrap(),
        DestinationPin::new(
            "org-1",
            "https://gateway.example.invalid",
            "subject-1",
            "other-agent",
        )
        .unwrap(),
    ] {
        assert!(outbox.enqueue(&destination, &command("cmd")).is_err());
        assert!(outbox.pending_for(&destination).unwrap().is_empty());
    }
    assert_eq!(outbox.enqueue(&original, &command("cmd")).unwrap(), first);
    assert_eq!(outbox.pending_for(&original).unwrap().len(), 1);
}

#[test]
fn full_custody_refuses_new_ids_but_retains_rows_and_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("outbox");
    let outbox = ResultOutbox::open(&dir).unwrap();
    let destination = pin("org");
    let first = outbox.enqueue(&destination, &command("cmd-0")).unwrap();
    for n in 1..MAX_PENDING {
        outbox
            .enqueue(&destination, &command(&format!("cmd-{n}")))
            .unwrap();
    }
    assert!(outbox.enqueue(&destination, &command("overflow")).is_err());
    assert_eq!(
        outbox.enqueue(&destination, &command("cmd-0")).unwrap(),
        first
    );
    drop(outbox);
    assert_eq!(
        ResultOutbox::open(&dir)
            .unwrap()
            .pending_for(&destination)
            .unwrap()
            .len(),
        MAX_PENDING
    );
}

#[test]
fn concurrent_new_ids_cannot_cross_capacity() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("outbox");
    let outbox = ResultOutbox::open(&dir).unwrap();
    for n in 0..MAX_PENDING - 1 {
        outbox
            .enqueue(&pin("org"), &command(&format!("cmd-{n}")))
            .unwrap();
    }
    let barrier = Arc::new(Barrier::new(2));
    let contenders: Vec<_> = (0..2)
        .map(|n| {
            let dir = dir.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let independent = ResultOutbox::open(&dir).unwrap();
                barrier.wait();
                independent.enqueue(&pin("org"), &command(&format!("new-{n}")))
            })
        })
        .collect();
    let results: Vec<_> = contenders.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(outbox.pending_for(&pin("org")).unwrap().len(), MAX_PENDING);
}

#[test]
fn altered_stored_payload_is_retained_but_never_reported_as_valid_custody() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("outbox");
    let outbox = ResultOutbox::open(&dir).unwrap();
    outbox.enqueue(&pin("org"), &command("cmd")).unwrap();
    drop(outbox);
    let conn = rusqlite::Connection::open(dir.join("results.sqlite3")).unwrap();
    conn.execute("UPDATE pending_results SET digest='wrong'", [])
        .unwrap();
    drop(conn);
    let reopened = ResultOutbox::open(&dir).unwrap();
    assert!(reopened.pending_for(&pin("org")).is_err());
    assert!(reopened.enqueue(&pin("org"), &command("cmd")).is_err());
    let conn = rusqlite::Connection::open(dir.join("results.sqlite3")).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM pending_results", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn private_paths_and_foreign_database_refusal_leave_other_data_untouched() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("outbox");
    drop(ResultOutbox::open(&dir).unwrap());
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(dir.join("results.sqlite3"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(ResultOutbox::open(std::path::Path::new("relative")).is_err());
    let linked = root.path().join("linked");
    std::os::unix::fs::symlink(&dir, &linked).unwrap();
    assert!(ResultOutbox::open(&linked).is_err());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ResultOutbox::open(&dir).is_err());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let db = dir.join("results.sqlite3");
    fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(ResultOutbox::open(&dir).is_err());
    fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
    let external = root.path().join("external.sqlite3");
    fs::rename(&db, &external).unwrap();
    std::os::unix::fs::symlink(&external, &db).unwrap();
    assert!(ResultOutbox::open(&dir).is_err());
    let foreign = root.path().join("foreign");
    fs::DirBuilder::new().mode(0o700).create(&foreign).unwrap();
    let path = foreign.join("results.sqlite3");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE unrelated(value TEXT); INSERT INTO unrelated VALUES ('keep');",
    )
    .unwrap();
    drop(conn);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::read(&path).unwrap();
    let before_sha = Sha256::digest(&before);
    assert!(ResultOutbox::open(&foreign).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(Sha256::digest(fs::read(&path).unwrap()), before_sha);
}

#[test]
fn copied_header_wrong_schema_or_trigger_is_refused_without_database_changes() {
    let root = tempfile::tempdir().unwrap();
    for (name, mutation) in [
        ("wrong-schema", "DROP TABLE pending_results; CREATE TABLE pending_results(command_id INTEGER, destination BLOB, payload TEXT, digest TEXT, stored_at_ms TEXT)"),
        ("trigger", "CREATE TRIGGER unexpected AFTER INSERT ON pending_results BEGIN UPDATE pending_results SET digest='changed' WHERE command_id=NEW.command_id; END"),
    ] {
        let dir = root.path().join(name);
        drop(ResultOutbox::open(&dir).unwrap());
        let path = dir.join("results.sqlite3");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(mutation).unwrap();
        drop(conn);
        let before = fs::read(&path).unwrap();
        let hash = Sha256::digest(&before);
        assert!(ResultOutbox::open(&dir).is_err(), "{name}");
        assert_eq!(fs::read(&path).unwrap(), before, "{name}");
        assert_eq!(Sha256::digest(fs::read(&path).unwrap()), hash, "{name}");
    }
}

#[test]
fn held_initialization_lock_refuses_with_a_bounded_wait() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("outbox");
    drop(ResultOutbox::open(&dir).unwrap());
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("init.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let (send, receive) = std::sync::mpsc::channel();
    let worker_dir = dir.clone();
    let worker = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let refused = ResultOutbox::open(&worker_dir).is_err();
        send.send((refused, start.elapsed())).unwrap();
    });
    // On a missing bounded guard, release our own lock so the fixture never
    // strands the worker. A delayed success still fails the assertion below.
    let observed = receive.recv_timeout(std::time::Duration::from_secs(7));
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    worker.join().unwrap();
    let (refused, elapsed) = observed.expect("initialization exceeded its five-second budget");
    assert!(refused);
    assert!(elapsed >= std::time::Duration::from_secs(4));
    assert!(elapsed < std::time::Duration::from_secs(7));
    assert!(ResultOutbox::open(&dir).is_ok());
}

#[test]
fn fifo_database_path_is_refused_before_a_blocking_read_open() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("outbox");
    fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let path = dir.join("results.sqlite3");
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let (send, receive) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        send.send(ResultOutbox::open(&dir).is_err()).unwrap();
    });
    let observed = receive.recv_timeout(std::time::Duration::from_secs(1));
    // If O_NONBLOCK was removed, our own descriptor releases the blocked
    // reader, letting the regression fail promptly rather than hanging CI.
    let release = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&path)
        .unwrap();
    worker.join().unwrap();
    drop(release);
    assert!(observed.expect("FIFO validation blocked before checking file type"));
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_fifo());
}

#[test]
fn abrupt_exit_rolls_back_a_later_write_and_retains_committed_custody() {
    const CHILD_DIR: &str = "CADENCE_OFFLINE_OUTBOX_CRASH_FIXTURE";
    if let Some(dir) = std::env::var_os(CHILD_DIR) {
        let dir = std::path::PathBuf::from(dir);
        let outbox = ResultOutbox::open(&dir).unwrap();
        outbox.enqueue(&pin("org"), &command("committed")).unwrap();
        drop(outbox);
        let conn = rusqlite::Connection::open(dir.join("results.sqlite3")).unwrap();
        if let Ok(mode) = std::env::var("CADENCE_OFFLINE_OUTBOX_CRASH_SCHEMA") {
            match mode.as_str() {
                "wrong-schema" => conn.execute_batch("DROP TABLE pending_results; CREATE TABLE pending_results(command_id TEXT, destination TEXT, payload TEXT, digest TEXT, stored_at_ms INTEGER)").unwrap(),
                "trigger" => conn.execute_batch("CREATE TRIGGER unexpected AFTER INSERT ON pending_results BEGIN UPDATE pending_results SET digest='changed' WHERE command_id=NEW.command_id; END").unwrap(),
                _ => panic!("unknown fixture mode"),
            }
        }
        conn.execute_batch("PRAGMA cache_size=1; BEGIN IMMEDIATE")
            .unwrap();
        for n in 0..40 {
            conn.execute(
                "INSERT INTO pending_results VALUES (?, ?, ?, ?, ?)",
                rusqlite::params![
                    format!("uncommitted-{n}"),
                    "dirty",
                    "x".repeat(32_768),
                    "dirty",
                    1
                ],
            )
            .unwrap();
        }
        // A real abrupt child exit omits Connection drop/transaction rollback.
        std::process::exit(73);
    }
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("outbox");
    let output = cadence_agent::reaper::output(
        std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("abrupt_exit_rolls_back_a_later_write_and_retains_committed_custody")
            .arg("--test-threads=1")
            .env(CHILD_DIR, &dir),
    )
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(73),
        "crash fixture did not reach its abrupt exit"
    );
    assert!(dir.join("results.sqlite3-journal").is_file());
    let recovered = ResultOutbox::open(&dir).unwrap();
    let rows = recovered.pending_for(&pin("org")).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].receipt().command_id(), "committed");
    assert_eq!(rows[0].command(), &command("committed"));
    assert_eq!(
        recovered
            .enqueue(&pin("org"), &command("committed"))
            .unwrap(),
        *rows[0].receipt()
    );
}

#[test]
fn hot_journal_foreign_schemas_preserve_database_and_journal_bytes() {
    let root = tempfile::tempdir().unwrap();
    for mode in ["wrong-schema", "trigger"] {
        let dir = root.path().join(mode);
        let output = cadence_agent::reaper::output(
            std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("abrupt_exit_rolls_back_a_later_write_and_retains_committed_custody")
                .arg("--test-threads=1")
                .env("CADENCE_OFFLINE_OUTBOX_CRASH_FIXTURE", &dir)
                .env("CADENCE_OFFLINE_OUTBOX_CRASH_SCHEMA", mode),
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(73), "{mode}");
        let db = dir.join("results.sqlite3");
        let journal = dir.join("results.sqlite3-journal");
        let db_bytes = fs::read(&db).unwrap();
        let journal_bytes = fs::read(&journal).unwrap();
        assert!(ResultOutbox::open(&dir).is_err(), "{mode}");
        assert_eq!(fs::read(&db).unwrap(), db_bytes, "{mode}");
        assert_eq!(fs::read(&journal).unwrap(), journal_bytes, "{mode}");
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            3,
            "no recovery scratch is retained"
        );
    }
}

#[test]
fn unsafe_existing_journal_and_wal_paths_are_refused_without_touching_targets() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("outbox");
    drop(ResultOutbox::open(&dir).unwrap());
    let target = root.path().join("untouched");
    fs::write(&target, b"keep foreign bytes").unwrap();
    let journal = dir.join("results.sqlite3-journal");
    std::os::unix::fs::symlink(&target, &journal).unwrap();
    assert!(ResultOutbox::open(&dir).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"keep foreign bytes");
    fs::remove_file(&journal).unwrap();
    fs::write(dir.join("results.sqlite3-wal"), b"unsupported WAL").unwrap();
    assert!(ResultOutbox::open(&dir).is_err());
    assert_eq!(
        fs::read(dir.join("results.sqlite3-wal")).unwrap(),
        b"unsupported WAL"
    );
}
