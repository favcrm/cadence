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
    let err = export(&state, &bundle, empty_pm().path(), false).expect_err("must refuse");
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
    let out = export(&state, &bundle, &pm, false).unwrap();
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
    export(&state, &bundle, empty_pm().path(), false).unwrap();
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

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn backup_copy_manifest_and_restored_db_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 2);
    // A dest that already exists with open permissions (outside state).
    let dest = tmp.path().join("open");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The process umask (022 under cargo) alone would leave 0644.
    let out = backup(&state, Some(&dest), 3).unwrap();
    let copy = PathBuf::from(out["backup"].as_str().unwrap());
    let manifest = PathBuf::from(out["manifest_path"].as_str().unwrap());
    assert_eq!(mode(&copy), 0o600);
    assert_eq!(mode(&manifest), 0o600);

    let target = tmp.path().join("restored");
    restore(&target, &manifest).unwrap();
    assert_eq!(mode(&db_file(&target)), 0o600);
}

/// A plausible-looking backup of ours in `dest`, dated `stamp`, with a
/// manifest claiming `epoch`.
fn plant_ours(dest: &Path, stamp: &str, epoch: f64) -> PathBuf {
    let stem = format!("{FILE_PREFIX}{stamp}-cafecafe");
    let file = format!("{stem}{DB_SUFFIX}");
    std::fs::write(dest.join(&file), b"old copy").unwrap();
    let manifest = Manifest {
        kind: MANIFEST_KIND.into(),
        file: file.clone(),
        sha256: String::new(),
        bytes: 8,
        schema_version: SCHEMA_VERSION,
        build_commit: String::new(),
        created_at: String::new(),
        created_at_epoch: epoch,
        source: String::new(),
    };
    std::fs::write(
        dest.join(format!("{stem}{MANIFEST_SUFFIX}")),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    dest.join(file)
}

#[test]
fn prune_never_removes_the_copy_just_taken_even_against_future_dated_backups() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 2);
    let dest = default_dest(&state);
    std::fs::create_dir_all(&dest).unwrap();
    let a = plant_ours(&dest, "20991231T235959Z", 4.0e12);
    // keep 1: only the new copy survives, the future-dated one goes.
    let out = pre_update(&state).unwrap();
    assert!(PathBuf::from(out["backup"].as_str().unwrap()).is_file());
    let out = backup(&state, None, 1).unwrap();
    let newest = PathBuf::from(out["backup"].as_str().unwrap());
    assert!(newest.is_file(), "the copy just taken was pruned");
    assert!(!a.exists());
    // keep 2: the new copy plus the newest other one by name stamp.
    let b = plant_ours(&dest, "20991231T235958Z", 1.0);
    let out = backup(&state, None, 2).unwrap();
    assert!(PathBuf::from(out["backup"].as_str().unwrap()).is_file());
    assert!(b.exists(), "future stamp sorts newest among the others");
    assert!(!newest.exists());
}

#[test]
fn export_scans_the_database_and_refuses_a_secret_in_message_history() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let live = live_db(&state, SCHEMA_VERSION, 1);
    live.execute_batch(
        "CREATE TABLE messages(seq INTEGER PRIMARY KEY, body TEXT NOT NULL, result TEXT, error TEXT);",
    )
    .unwrap();
    live.execute(
        "INSERT INTO messages(body) VALUES ('hello'), (?1)",
        [format!("deploy with\n{}\n", github_pat())],
    )
    .unwrap();
    let bundle = tmp.path().join("b.tar");
    let text = export(&state, &bundle, empty_pm().path(), false)
        .expect_err("must refuse")
        .to_string();
    assert!(text.contains("messages.body rowid 2"), "{text}");
    assert!(!text.contains(&github_pat()));
    assert!(!bundle.exists() && !sibling_partial(&bundle).exists());

    // The explicit override exports and says so.
    let out = export(&state, &bundle, empty_pm().path(), true).unwrap();
    assert_eq!(out["secret_scan"]["unscanned"], json!(["cadence.sqlite3"]));
    assert!(bundle.is_file());
}

#[test]
fn export_reports_scanned_db_columns_when_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let live = live_db(&state, SCHEMA_VERSION, 1);
    live.execute_batch(
        "CREATE TABLE events(seq INTEGER PRIMARY KEY, payload TEXT NOT NULL);
         INSERT INTO events(payload) VALUES ('{}'), ('{\"k\":1}');",
    )
    .unwrap();
    let out = export(&state, &tmp.path().join("b.tar"), empty_pm().path(), false).unwrap();
    assert_eq!(out["secret_scan"]["db_columns"], json!(["events.payload"]));
    assert_eq!(out["secret_scan"]["db_values_scanned"], 2);
    assert_eq!(out["secret_scan"]["unscanned"], json!([]));
}

#[test]
fn export_refuses_an_oversized_text_member() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 1);
    std::fs::create_dir_all(state.join("briefings/pm")).unwrap();
    std::fs::write(
        state.join("briefings/pm/BRIEFING-big.md"),
        vec![b'a'; TEXT_MEMBER_MAX as usize + 1],
    )
    .unwrap();
    let bundle = tmp.path().join("b.tar");
    let text = message(export(&state, &bundle, empty_pm().path(), false));
    assert!(text.contains("cap"), "{text}");
    assert!(!bundle.exists());
}

#[test]
fn export_never_overwrites_an_existing_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 1);
    let bundle = tmp.path().join("b.tar");
    std::fs::write(&bundle, "mine").unwrap();
    assert!(message(export(&state, &bundle, empty_pm().path(), false)).contains("never overwrites"));
    assert_eq!(std::fs::read(&bundle).unwrap(), b"mine");
}

#[test]
fn restore_refuses_a_copy_that_fails_integrity_check() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 400);
    let out = backup(&state, None, DEFAULT_KEEP).unwrap();
    let copy = PathBuf::from(out["backup"].as_str().unwrap());
    let manifest_path = PathBuf::from(out["manifest_path"].as_str().unwrap());
    // Trash every page after the first, then re-sign the manifest so
    // only the integrity check can catch it.
    let mut bytes = std::fs::read(&copy).unwrap();
    assert!(bytes.len() > 8192);
    for b in &mut bytes[4096..] {
        *b = 0x5a;
    }
    std::fs::write(&copy, &bytes).unwrap();
    let mut manifest: Manifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest.sha256 = sha256_file(&copy).unwrap().0;
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    let target = tmp.path().join("t");
    let text = message(restore(&target, &manifest_path));
    assert!(
        text.contains("integrity_check") || text.contains("failed verification"),
        "{text}"
    );
    assert!(!db_file(&target).exists());
}

/// Append a member with a raw (unsanitised) name, as a hostile tool would.
fn raw_member(tar: &mut tar::Builder<File>, name: &str, kind: tar::EntryType, link: Option<&str>) {
    let mut header = tar::Header::new_old();
    let old = header.as_old_mut();
    old.name[..name.len()].copy_from_slice(name.as_bytes());
    if let Some(link) = link {
        old.linkname[..link.len()].copy_from_slice(link.as_bytes());
    }
    let data: &[u8] = if link.is_some() { b"" } else { b"x" };
    header.set_size(data.len() as u64);
    header.set_mode(0o600);
    header.set_entry_type(kind);
    header.set_cksum();
    tar.append(&header, data).unwrap();
}

#[test]
fn restore_refuses_traversal_absolute_and_symlink_members() {
    let tmp = tempfile::tempdir().unwrap();
    let cases: [(&str, tar::EntryType, Option<&str>); 4] = [
        ("briefings/../../escape.md", tar::EntryType::Regular, None),
        ("/tmp/abs-escape.md", tar::EntryType::Regular, None),
        (
            "briefings/pm/link.md",
            tar::EntryType::Symlink,
            Some("/etc/passwd"),
        ),
        (
            "briefings/pm/hard.md",
            tar::EntryType::Link,
            Some("cadence.sqlite3"),
        ),
    ];
    for (i, (name, kind, link)) in cases.into_iter().enumerate() {
        let bundle = tmp.path().join(format!("evil-{i}.tar"));
        let mut tar = tar::Builder::new(File::create(&bundle).unwrap());
        raw_member(&mut tar, name, kind, link);
        tar.into_inner().unwrap();
        let target = tmp.path().join(format!("t{i}"));
        let text = message(restore(&target, &bundle));
        assert!(text.contains("unexpected member"), "{name}: {text}");
        assert!(!db_file(&target).exists());
    }
    assert!(!tmp.path().join("escape.md").exists());
}

#[test]
fn restore_refuses_a_symlinked_briefings_dir_before_installing_the_db() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _live = live_db(&state, SCHEMA_VERSION, 2);
    std::fs::create_dir_all(state.join("briefings/pm")).unwrap();
    std::fs::write(state.join("briefings/pm/BRIEFING-w1.md"), "hi\n").unwrap();
    let bundle = tmp.path().join("b.tar");
    export(&state, &bundle, empty_pm().path(), false).unwrap();

    let target = tmp.path().join("t");
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, target.join("briefings")).unwrap();
    let text = message(restore(&target, &bundle));
    assert!(text.contains("symlink"), "{text}");
    assert!(!db_file(&target).exists(), "refused before the db install");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}
