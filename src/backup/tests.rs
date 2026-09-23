//! Tokens here are built at runtime; no literal has a credential shape.

use super::*;
use std::os::unix::io::AsRawFd;

/// A live WAL database: `schema_version` = `schema`, `n` rows in
/// `items`, and the connection kept open with autocheckpoint off so the
/// rows sit in the `-wal` when the backup runs.
fn live_db(state: &Path, schema: i64, n: i64) -> Connection {
    std::fs::create_dir_all(state).unwrap();
    let conn = Connection::open(db_file(state)).unwrap();
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
        .unwrap();
    conn.execute_batch(
        "PRAGMA wal_autocheckpoint=0;
         CREATE TABLE schema_version(version INTEGER NOT NULL);
         CREATE TABLE items(id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    )
    .unwrap();
    conn.execute("INSERT INTO schema_version VALUES (?1)", [schema])
        .unwrap();
    for i in 0..n {
        conn.execute("INSERT INTO items(body) VALUES (?1)", [format!("row {i}")])
            .unwrap();
    }
    conn
}

fn rows(path: &Path) -> Vec<String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut stmt = conn.prepare("SELECT body FROM items ORDER BY id").unwrap();
    let out = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<Vec<String>, _>>()
        .unwrap();
    out
}

fn wal_len(state: &Path) -> u64 {
    let wal = format!("{}-wal", db_file(state).display());
    std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0)
}

fn message(result: Result<Value>) -> String {
    result.expect_err("expected a refusal").to_string()
}

fn empty_pm() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn backup_of_live_wal_db_is_integrity_ok_with_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 50);
    assert!(wal_len(&state) > 0, "rows must be in the WAL for this test");

    let out = backup(&state, None, DEFAULT_KEEP).unwrap();
    let copy = PathBuf::from(out["backup"].as_str().unwrap());
    assert!(copy.starts_with(state.join("backups")));
    assert_eq!(verify_integrity(&copy).unwrap(), Some(SCHEMA_VERSION));
    assert_eq!(rows(&copy).len(), 50, "the snapshot includes WAL-only rows");

    let manifest_path = PathBuf::from(out["manifest_path"].as_str().unwrap());
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let (sha, bytes) = sha256_file(&copy).unwrap();
    assert_eq!(manifest.kind, MANIFEST_KIND);
    assert_eq!(manifest.sha256, sha);
    assert_eq!(manifest.bytes, bytes);
    assert_eq!(manifest.schema_version, SCHEMA_VERSION);
    assert_eq!(manifest.build_commit, crate::overview::BUILD_COMMIT);
    assert_eq!(manifest.source, db_file(&state).display().to_string());
    assert_eq!(
        Path::new(&manifest.file),
        Path::new(copy.file_name().unwrap())
    );
    // The copy stands alone: rollback journal, no sidecars needed.
    let header = std::fs::read(&copy).unwrap();
    assert_eq!(&header[18..20], &[1, 1], "copy is not in WAL mode");
    assert!(wal_len(&state) > 0, "backup must not checkpoint the source");
}

#[test]
fn backup_refuses_missing_database() {
    let tmp = tempfile::tempdir().unwrap();
    let text = message(backup(tmp.path(), None, 3));
    assert!(text.contains("nothing to back up"), "{text}");
}

#[test]
fn keep_prunes_only_our_backups() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 3);
    let dest = tmp.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    // Files we did not create, including look-alikes.
    let foreign = [
        ("notes.txt", "keep me".to_string()),
        ("cadence-backup-orphan.sqlite3", "no manifest".to_string()),
        (
            "cadence-backup-19700101T000000Z-aaaaaaaa.manifest.json",
            json!({"kind": "someone-else/1",
                   "file": "cadence-backup-19700101T000000Z-aaaaaaaa.sqlite3",
                   "created_at_epoch": 0.0})
            .to_string(),
        ),
        (
            "cadence-backup-19700101T000000Z-aaaaaaaa.sqlite3",
            "foreign copy".to_string(),
        ),
        (
            "cadence-backup-19700101T000000Z-bbbbbbbb.manifest.json",
            // Our kind, but it names a file that is not its sibling.
            serde_json::to_string(&Manifest {
                kind: MANIFEST_KIND.into(),
                file: "notes.txt".into(),
                sha256: String::new(),
                bytes: 0,
                schema_version: 1,
                build_commit: String::new(),
                created_at: String::new(),
                created_at_epoch: 0.0,
                source: String::new(),
            })
            .unwrap(),
        ),
    ];
    for (name, body) in &foreign {
        std::fs::write(dest.join(name), body).unwrap();
    }

    let mut made = Vec::new();
    for _ in 0..4 {
        let out = backup(&state, Some(&dest), 2).unwrap();
        made.push(PathBuf::from(out["backup"].as_str().unwrap()));
    }
    for (name, _) in &foreign {
        assert!(dest.join(name).exists(), "{name} was not ours to delete");
    }
    let ours: Vec<String> = std::fs::read_dir(&dest)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| our_manifest(&dest, n).is_some())
        .collect();
    assert_eq!(ours.len(), 2, "{ours:?}");
    assert!(!made[0].exists() && !made[1].exists());
    assert!(made[2].exists() && made[3].exists());
}

#[test]
fn keep_zero_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let _live = live_db(tmp.path(), SCHEMA_VERSION, 1);
    assert!(message(backup(tmp.path(), None, 0)).contains("--keep"));
}

fn github_pat() -> String {
    // Built at runtime: prefix + deterministic noise.
    let noise: String = (0..36)
        .map(|i| (b'a' + ((i * 7 + 3) % 26) as u8) as char)
        .map(|c| {
            if (c as u8).is_multiple_of(3) {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect();
    ["gh", "p_", &noise].concat()
}

#[test]
fn export_refuses_a_planted_secret_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 2);
    let dir = state.join("briefings/pm");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("BRIEFING-w1.md"),
        format!("use this token:\n{}\n", github_pat()),
    )
    .unwrap();
    let bundle = tmp.path().join("out.tar");
    let err = export(&state, &bundle, empty_pm().path()).expect_err("must refuse");
    let text = err.to_string();
    assert!(text.contains("briefings/pm/BRIEFING-w1.md"), "{text}");
    assert!(
        !text.contains(&github_pat()),
        "the refusal must not echo the value"
    );
    assert!(!bundle.exists());
    assert!(!sibling_partial(&bundle).exists());
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn export_restore_round_trip_reproduces_rows_and_plans_remap() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 25);
    let brief = state.join("briefings/pm");
    std::fs::create_dir_all(&brief).unwrap();
    std::fs::write(brief.join("BRIEFING-w1.md"), "be kind\n").unwrap();
    std::fs::create_dir_all(state.join("logs")).unwrap();
    std::fs::write(state.join("logs/provider.log"), "provider output\n").unwrap();

    // Tracker with one checkout that exists and one that does not.
    let checkout = tmp.path().join("app");
    std::fs::create_dir_all(&checkout).unwrap();
    git(&checkout, &["init", "-q"]);
    git(
        &checkout,
        &["remote", "add", "origin", "git@github.com:acme/app.git"],
    );
    let pm = tmp.path().join("pm");
    std::fs::create_dir_all(pm.join("app")).unwrap();
    std::fs::write(
        pm.join("app/project.yaml"),
        format!(
            "key: app\nprefix: APP\nrepos:\n  - remote: https://github.com/acme/app\n    path: {}\n  - remote: https://github.com/acme/gone\n    path: {}\n",
            checkout.display(),
            tmp.path().join("gone").display()
        ),
    )
    .unwrap();

    let bundle = tmp.path().join("b.tar");
    let out = export(&state, &bundle, &pm).unwrap();
    let members: Vec<&str> = out["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    assert_eq!(
        members,
        [
            "cadence.sqlite3",
            "manifest.json",
            "repo-map.json",
            "briefings/pm/BRIEFING-w1.md"
        ]
    );
    let text_before = std::fs::read(tmp.path().join("pm/app/project.yaml")).unwrap();

    let target = tmp.path().join("restored");
    let out = restore(&target, &bundle).unwrap();
    assert_eq!(rows(&db_file(&target)).len(), 25);
    assert_eq!(rows(&db_file(&target)), rows(&db_file(&state)));
    assert_eq!(
        std::fs::read_to_string(target.join("briefings/pm/BRIEFING-w1.md")).unwrap(),
        "be kind\n"
    );
    assert!(
        !target.join("logs").exists(),
        "provider logs are not bundled"
    );
    let remap = out["repo_remap"].as_array().unwrap();
    assert_eq!(remap.len(), 2);
    assert_eq!(remap[0]["action"], "found");
    assert_eq!(remap[0]["local_path"], json!(checkout));
    assert_eq!(remap[1]["action"], "choose folder");
    // The tracker is untouched.
    assert_eq!(
        std::fs::read(tmp.path().join("pm/app/project.yaml")).unwrap(),
        text_before
    );
    // No staging dir is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&target)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".restore-"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn restore_from_a_backup_manifest_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 7);
    let out = backup(&state, None, DEFAULT_KEEP).unwrap();
    let manifest = PathBuf::from(out["manifest_path"].as_str().unwrap());
    let target = tmp.path().join("restored");
    restore(&target, &manifest).unwrap();
    assert_eq!(rows(&db_file(&target)), rows(&db_file(&state)));
}

fn bundle_of(tmp: &Path, schema: i64) -> PathBuf {
    let state = tmp.join(format!("src-{schema}"));
    let live = live_db(&state, schema, 3);
    let bundle = tmp.join(format!("b-{schema}.tar"));
    export(&state, &bundle, empty_pm().path()).unwrap();
    drop(live);
    bundle
}

#[test]
fn restore_refuses_a_running_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = bundle_of(tmp.path(), SCHEMA_VERSION);
    let target = tmp.path().join("busy");
    std::fs::create_dir_all(&target).unwrap();
    let lock = File::create(target.join("cadence.lock")).unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let text = message(restore(&target, &bundle));
    assert!(text.contains("is running"), "{text}");
    assert!(!db_file(&target).exists());
}

#[test]
fn restore_refuses_an_existing_database() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = bundle_of(tmp.path(), SCHEMA_VERSION);
    let target = tmp.path().join("used");
    let existing = live_db(&target, SCHEMA_VERSION, 1);
    drop(existing);
    let before = std::fs::read(db_file(&target)).unwrap();
    let text = message(restore(&target, &bundle));
    assert!(text.contains("never overwrites"), "{text}");
    assert_eq!(std::fs::read(db_file(&target)).unwrap(), before);
}

#[test]
fn restore_refuses_a_newer_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = bundle_of(tmp.path(), SCHEMA_VERSION + 1);
    let target = tmp.path().join("new");
    let text = message(restore(&target, &bundle));
    assert!(text.contains("cadence upgrade"), "{text}");
    assert!(!db_file(&target).exists());
}

#[test]
fn restore_refuses_a_bad_checksum() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 3);
    let out = backup(&state, None, DEFAULT_KEEP).unwrap();
    let copy = PathBuf::from(out["backup"].as_str().unwrap());
    let mut bytes = std::fs::read(&copy).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&copy, bytes).unwrap();
    let manifest = PathBuf::from(out["manifest_path"].as_str().unwrap());
    let target = tmp.path().join("t");
    let text = message(restore(&target, &manifest));
    assert!(text.contains("checksum mismatch"), "{text}");
    assert!(!db_file(&target).exists());
}

#[test]
fn restore_refuses_a_bundle_with_unexpected_members() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("evil.tar");
    let mut tar = tar::Builder::new(File::create(&bundle).unwrap());
    append(&mut tar, "logs/provider.log", b"x", 0.0).unwrap();
    tar.into_inner().unwrap();
    let text = message(restore(&tmp.path().join("t"), &bundle));
    assert!(text.contains("unexpected member"), "{text}");
}

#[test]
fn pre_update_skips_without_a_database_and_backs_up_with_one() {
    let tmp = tempfile::tempdir().unwrap();
    let out = pre_update(tmp.path()).unwrap();
    assert!(out["skipped"].is_string());
    let _live = live_db(tmp.path(), SCHEMA_VERSION, 1);
    let out = pre_update(tmp.path()).unwrap();
    assert!(PathBuf::from(out["backup"].as_str().unwrap()).is_file());
}
