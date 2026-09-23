//! CAD-314: backup, export and restore. Every state dir, repo and bundle
//! lives in a temp dir. Tokens are built at runtime from a prefix plus
//! seeded noise, so no literal here has a credential shape.

use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{params, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::*;
use crate::rollout::SCHEMA_VERSION;
use crate::store::Store;

const ALPHANUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

fn noise(seed: &str, n: usize) -> String {
    let mut out = String::with_capacity(n);
    let mut counter = 0u32;
    while out.len() < n {
        for b in Sha256::digest(format!("{seed}:{counter}").as_bytes()) {
            if out.len() == n {
                break;
            }
            out.push(ALPHANUM[b as usize % ALPHANUM.len()] as char);
        }
        counter += 1;
    }
    out
}

/// A GitHub classic PAT shape: `ghp_` plus 36 characters.
fn github_token(seed: &str) -> String {
    [&["gh", "p_"].concat(), noise(seed, 36).as_str()].concat()
}

fn live(state: &Path) -> PathBuf {
    state.join("cadence.sqlite3")
}

/// A current-schema store at `<state>/cadence.sqlite3`, closed again.
fn fresh_state(root: &Path, name: &str) -> PathBuf {
    let state = root.join(name);
    std::fs::create_dir_all(&state).unwrap();
    drop(Store::open(&live(&state)).unwrap());
    state
}

fn writer(state: &Path) -> Connection {
    let conn = Connection::open(live(state)).unwrap();
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    conn
}

fn add_agent(conn: &Connection, alias: &str, cwd: &str) {
    conn.execute(
        "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,
                            generation,pid,created,updated)
         VALUES(?1,'codex','stdio','worker',?2,'workspace-write','idle','gen-1',4242,1,1)",
        params![alias, cwd],
    )
    .unwrap();
}

fn add_message(conn: &Connection, id: &str, alias: &str, body: &str) {
    conn.execute(
        "INSERT INTO messages(id,alias,body,source,state,created)
         VALUES(?1,?2,?3,'cli','completed',1)",
        params![id, alias, body],
    )
    .unwrap();
}

fn count(db: &Path, sql: &str) -> i64 {
    Connection::open(db)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

fn text(db: &Path, sql: &str) -> Option<String> {
    Connection::open(db)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

fn sha256_file(path: &Path) -> String {
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// `git init` plus an `origin`. No commits, so no identity is needed.
fn git_repo(dir: &Path, origin: &str) {
    std::fs::create_dir_all(dir).unwrap();
    for args in [
        vec!["-c", "init.defaultBranch=main", "init", "-q"],
        vec!["remote", "add", "origin", origin],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

fn manifests(dir: &Path) -> Vec<Value> {
    let mut out: Vec<Value> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".manifest.json"))
        .map(|e| read_json(&e.path()))
        .collect();
    out.sort_by(|a, b| a["created_at"].as_str().cmp(&b["created_at"].as_str()));
    out
}

#[test]
fn cad314_backup_writes_verified_copy_and_manifest() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let repo = root.path().join("repo");
    git_repo(&repo, "https://github.com/favcrm/cadence.git");
    {
        let conn = writer(&state);
        add_agent(&conn, "w1", repo.to_str().unwrap());
        add_message(&conn, "m1", "w1", "hello");
    }
    let dir = root.path().join("backups");

    let out = backup(&state, &dir, DEFAULT_KEEP, "manual").unwrap();

    let manifest_path = PathBuf::from(out["manifest"].as_str().unwrap());
    let db = PathBuf::from(out["db"].as_str().unwrap());
    assert_eq!(manifest_path.parent(), Some(dir.as_path()));
    let m = read_json(&manifest_path);
    assert_eq!(m["format"], "cadence.backup/1");
    assert_eq!(m["kind"], "backup");
    assert_eq!(m["reason"], "manual");
    assert_eq!(m["schema_version"], SCHEMA_VERSION);
    assert_eq!(m["integrity_check"], "ok");
    assert_eq!(m["sha256"], sha256_file(&db));
    assert_eq!(m["bytes"], std::fs::metadata(&db).unwrap().len());
    assert_eq!(m["db_file"], db.file_name().unwrap().to_str().unwrap());
    assert_eq!(m["versions"]["binary_schema"], SCHEMA_VERSION);
    assert!(m["versions"]["cadence"]
        .as_str()
        .unwrap()
        .starts_with(env!("CARGO_PKG_VERSION")));
    assert!(!m["versions"]["sqlite"].as_str().unwrap().is_empty());
    let repos = m["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1, "{m}");
    assert_eq!(
        repos[0]["path"],
        repo.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(repos[0]["remote"], "https://github.com/favcrm/cadence.git");
    // A self-contained single file: no WAL sidecar, rows readable.
    assert!(!dir
        .join(format!("{}-wal", db.file_name().unwrap().to_str().unwrap()))
        .exists());
    assert_eq!(count(&db, "SELECT count(*) FROM messages"), 1);
    // Owner-only: the copy carries everything the live store does.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&db).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn cad314_backup_is_online_and_does_not_block_a_writer() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let conn = writer(&state);
    add_agent(&conn, "w1", "/nowhere");
    add_message(&conn, "committed", "w1", "one");
    // A writer mid-transaction, as the daemon would be.
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    add_message(&conn, "uncommitted", "w1", "two");

    let out = backup(&state, &root.path().join("b"), DEFAULT_KEEP, "manual").unwrap();

    let db = PathBuf::from(out["db"].as_str().unwrap());
    assert_eq!(count(&db, "SELECT count(*) FROM messages"), 1);
    // The writer was never blocked out and still commits.
    conn.execute_batch("COMMIT").unwrap();
    assert_eq!(count(&live(&state), "SELECT count(*) FROM messages"), 2);
}

#[test]
fn cad314_backup_retention_keeps_newest_n_per_reason_and_ignores_foreign_files() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let dir = root.path().join("backups");
    std::fs::create_dir_all(&dir).unwrap();
    let foreign = dir.join("cadence-live-20260921T072606Z.sqlite3");
    std::fs::write(&foreign, b"hand-made copy").unwrap();

    let manual = backup(&state, &dir, 2, "manual").unwrap();
    let mut pruned = Vec::new();
    for _ in 0..3 {
        let out = backup(&state, &dir, 2, "nightly").unwrap();
        pruned.extend(out["pruned"].as_array().unwrap().clone());
    }

    let all = manifests(&dir);
    let nightly: Vec<&Value> = all.iter().filter(|m| m["reason"] == "nightly").collect();
    assert_eq!(nightly.len(), 2, "{all:?}");
    assert_eq!(pruned.len(), 2, "one db + one manifest pruned: {pruned:?}");
    assert!(PathBuf::from(manual["manifest"].as_str().unwrap()).exists());
    assert!(PathBuf::from(manual["db"].as_str().unwrap()).exists());
    assert!(
        foreign.exists(),
        "a file without a cadence manifest is never pruned"
    );
    // Every surviving manifest still names an existing copy.
    for m in &all {
        assert!(dir.join(m["db_file"].as_str().unwrap()).exists());
    }
}

#[test]
fn cad314_export_bundle_holds_only_the_scrubbed_db_and_manifest() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    {
        let conn = writer(&state);
        add_agent(&conn, "w1", "/nowhere");
        add_message(&conn, "m1", "w1", "ordinary work");
    }
    // Credential-bearing neighbours of the store in the state dir.
    let marker = noise("env-marker", 24);
    std::fs::write(state.join(".env"), format!("MARKER={marker}\n")).unwrap();
    std::fs::write(state.join("ui.json"), format!("{{\"m\":\"{marker}\"}}")).unwrap();
    std::fs::write(state.join("intake-relay.yaml"), format!("m: {marker}\n")).unwrap();
    std::fs::write(state.join("secret-allowlist.toml"), "").unwrap();
    std::fs::create_dir_all(state.join("private")).unwrap();
    std::fs::write(state.join("private/token"), &marker).unwrap();
    let out_dir = root.path().join("bundle");

    let out = export(&state, &out_dir).unwrap();

    let mut names: Vec<String> = std::fs::read_dir(&out_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["cadence.sqlite3", "manifest.json"]);
    let m = read_json(&out_dir.join("manifest.json"));
    assert_eq!(m["kind"], "export");
    assert_eq!(m["sha256"], sha256_file(&out_dir.join("cadence.sqlite3")));
    let excludes = m["export"]["excludes"].to_string();
    for item in ["ui.json", ".env", "provider", "token"] {
        assert!(excludes.contains(item), "{item} not in {excludes}");
    }
    assert!(out["scan"]["cells"].as_u64().unwrap() > 0);
    let db = out_dir.join("cadence.sqlite3");
    assert_eq!(text(&db, "SELECT generation FROM agents"), None);
    assert_eq!(
        count(&db, "SELECT count(*) FROM agents WHERE pid IS NOT NULL"),
        0
    );
    assert_eq!(
        text(&db, "SELECT body FROM messages").as_deref(),
        Some("ordinary work")
    );
    for name in &names {
        let bytes = std::fs::read(out_dir.join(name)).unwrap();
        assert!(
            !contains(&bytes, &marker),
            "{name} carries a state-dir secret"
        );
    }
}

#[test]
fn cad314_export_fails_closed_on_a_secret_and_writes_nothing() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let token = github_token("export-hit");
    {
        let conn = writer(&state);
        add_agent(&conn, "w1", "/nowhere");
        add_message(&conn, "m1", "w1", &format!("pasted {token} by mistake"));
    }
    let out_dir = root.path().join("bundle");

    let err = export(&state, &out_dir).unwrap_err().to_string();

    assert!(err.contains("github-pat"), "{err}");
    assert!(err.contains("messages.body"), "names the location: {err}");
    assert!(!err.contains(&token), "the refusal never carries the value");
    assert!(!out_dir.exists(), "nothing written on a hit");
}

#[test]
fn cad314_export_drops_deleted_rows_left_in_free_pages() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let token = github_token("deleted-row");
    {
        let conn = writer(&state);
        conn.execute_batch("PRAGMA secure_delete=OFF").unwrap();
        add_agent(&conn, "w1", "/nowhere");
        add_message(
            &conn,
            "gone",
            "w1",
            &format!("{token} {}", "x".repeat(8000)),
        );
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        conn.execute("DELETE FROM messages WHERE id='gone'", [])
            .unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
    }
    // Precondition: the deleted value still sits in the live file's pages.
    assert!(contains(&std::fs::read(live(&state)).unwrap(), &token));
    let out_dir = root.path().join("bundle");

    export(&state, &out_dir).unwrap();

    let bytes = std::fs::read(out_dir.join("cadence.sqlite3")).unwrap();
    assert!(
        !contains(&bytes, &token),
        "freed pages were copied into the bundle"
    );
}

#[test]
fn cad314_backup_restore_round_trip_remaps_repo_paths_by_remote() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let old = root.path().join("old-host/cadence");
    let pw = noise("remote-pw", 12);
    git_repo(
        &old,
        &format!("https://someone:{pw}@github.com/Favcrm/Cadence.git"),
    );
    let old = old.canonicalize().unwrap();
    let lane = old.join(".worktrees/lane");
    std::fs::create_dir_all(&lane).unwrap();
    {
        let conn = writer(&state);
        add_agent(&conn, "w1", lane.to_str().unwrap());
        add_agent(&conn, "w2", "/somewhere/else");
        add_message(&conn, "m1", "w1", "one");
        add_message(&conn, "m2", "w2", "two");
        conn.execute(
            "INSERT INTO jobs(id,spec_path,pm_alias,repo,state,created,updated)
             VALUES('j1',?1,'pm',?2,'open',1,1)",
            params![old.join("spec.md").to_str().unwrap(), old.to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks(id,job_id,worktree,state,created,updated)
             VALUES('t1','j1',?1,'open',1,1)",
            params![old.join(".cadence/wt/t1").to_str().unwrap()],
        )
        .unwrap();
    }
    let backups = root.path().join("backups");
    let taken = backup(&state, &backups, DEFAULT_KEEP, "manual").unwrap();
    let manifest = PathBuf::from(taken["manifest"].as_str().unwrap());
    let m_text = std::fs::read_to_string(&manifest).unwrap();
    assert!(
        !m_text.contains(&pw),
        "remote credentials never reach the manifest"
    );
    assert!(m_text.contains("github.com/Favcrm/Cadence.git"), "{m_text}");

    // The new host cloned the same remote elsewhere, over ssh.
    let new = root.path().join("new-host/src/cadence");
    git_repo(&new, "git@github.com:favcrm/cadence.git");
    let new = new.canonicalize().unwrap();
    let target = root.path().join("restored");

    let out = restore(
        &manifest,
        &target,
        &RestoreOptions {
            force: false,
            repos: vec![new.clone()],
        },
    )
    .unwrap();

    let db = live(&target);
    let remapped = out["remapped"].as_array().unwrap();
    assert_eq!(remapped.len(), 1, "{out}");
    assert_eq!(remapped[0]["from"], old.to_str().unwrap());
    assert_eq!(remapped[0]["to"], new.to_str().unwrap());
    assert_eq!(remapped[0]["rows"], 4, "{out}");
    assert_eq!(
        text(&db, "SELECT cwd FROM agents WHERE alias='w1'").unwrap(),
        new.join(".worktrees/lane").to_str().unwrap()
    );
    assert_eq!(
        text(&db, "SELECT cwd FROM agents WHERE alias='w2'").unwrap(),
        "/somewhere/else"
    );
    assert_eq!(
        text(&db, "SELECT repo FROM jobs").unwrap(),
        new.to_str().unwrap()
    );
    assert_eq!(
        text(&db, "SELECT spec_path FROM jobs").unwrap(),
        new.join("spec.md").to_str().unwrap()
    );
    assert_eq!(
        text(&db, "SELECT worktree FROM tasks").unwrap(),
        new.join(".cadence/wt/t1").to_str().unwrap()
    );
    assert_eq!(count(&db, "SELECT count(*) FROM messages"), 2);
    // The daemon's own open path accepts the restored store.
    drop(Store::open(&db).unwrap());
}

#[test]
fn cad314_restore_refuses_a_running_daemon() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let taken = backup(&state, &root.path().join("b"), DEFAULT_KEEP, "manual").unwrap();
    let target = root.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    // Hold the daemon singleton exactly as `serve` does.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(target.join("cadence.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );

    let err = restore(
        Path::new(taken["manifest"].as_str().unwrap()),
        &target,
        &RestoreOptions {
            force: true,
            repos: vec![],
        },
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("daemon"), "{err}");
    assert!(!live(&target).exists());
    drop(lock);
}

#[test]
fn cad314_restore_refuses_existing_state_unless_forced() {
    let root = TempDir::new().unwrap();
    let source = fresh_state(root.path(), "source");
    writer(&source)
        .execute_batch("INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,created,updated) VALUES('from-backup','codex','stdio','worker','/x','ro','idle',1,1)")
        .unwrap();
    let taken = backup(&source, &root.path().join("b"), DEFAULT_KEEP, "manual").unwrap();
    let manifest = PathBuf::from(taken["manifest"].as_str().unwrap());
    let target = fresh_state(root.path(), "target");
    writer(&target)
        .execute_batch("INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,created,updated) VALUES('existing','codex','stdio','worker','/x','ro','idle',1,1)")
        .unwrap();

    let err = restore(&manifest, &target, &RestoreOptions::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("--force"), "{err}");
    assert_eq!(
        text(&live(&target), "SELECT alias FROM agents").as_deref(),
        Some("existing")
    );

    let out = restore(
        &manifest,
        &target,
        &RestoreOptions {
            force: true,
            repos: vec![],
        },
    )
    .unwrap();
    assert_eq!(
        text(&live(&target), "SELECT alias FROM agents").as_deref(),
        Some("from-backup")
    );
    // The replaced store was backed up first, and that copy is verified.
    let pre = PathBuf::from(out["pre_restore_backup"].as_str().unwrap());
    let pre_m = read_json(&pre);
    assert_eq!(pre_m["reason"], "pre-restore");
    let pre_db = pre
        .parent()
        .unwrap()
        .join(pre_m["db_file"].as_str().unwrap());
    assert_eq!(
        text(&pre_db, "SELECT alias FROM agents").as_deref(),
        Some("existing")
    );
    assert!(!target.join("cadence.sqlite3-wal").exists());
}

#[test]
fn cad314_restore_rejects_a_newer_schema() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let taken = backup(&state, &root.path().join("b"), DEFAULT_KEEP, "manual").unwrap();
    let manifest = PathBuf::from(taken["manifest"].as_str().unwrap());
    let db = PathBuf::from(taken["db"].as_str().unwrap());
    // A consistent bundle from a future binary: schema bumped, hash kept true.
    Connection::open(&db)
        .unwrap()
        .execute("UPDATE schema_version SET version=?1", [SCHEMA_VERSION + 1])
        .unwrap();
    let mut m = read_json(&manifest);
    m["schema_version"] = (SCHEMA_VERSION + 1).into();
    m["sha256"] = sha256_file(&db).into();
    m["bytes"] = std::fs::metadata(&db).unwrap().len().into();
    std::fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
    let target = root.path().join("target");

    let err = restore(&manifest, &target, &RestoreOptions::default())
        .unwrap_err()
        .to_string();

    assert!(err.contains("newer"), "{err}");
    assert!(!live(&target).exists());
}

#[test]
fn cad314_restore_rejects_a_copy_that_does_not_match_its_manifest() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let taken = backup(&state, &root.path().join("b"), DEFAULT_KEEP, "manual").unwrap();
    let db = PathBuf::from(taken["db"].as_str().unwrap());
    Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE smuggled(x)")
        .unwrap();
    let target = root.path().join("target");

    let err = restore(
        Path::new(taken["manifest"].as_str().unwrap()),
        &target,
        &RestoreOptions::default(),
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("sha256"), "{err}");
    assert!(!live(&target).exists());
}

#[test]
fn cad314_restore_refuses_two_checkouts_of_one_remote() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let old = root.path().join("old");
    git_repo(&old, "https://github.com/favcrm/cadence.git");
    writer(&state)
        .execute(
            "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,created,updated) VALUES('w','codex','stdio','worker',?1,'ro','idle',1,1)",
            [old.canonicalize().unwrap().to_str().unwrap()],
        )
        .unwrap();
    let taken = backup(&state, &root.path().join("b"), DEFAULT_KEEP, "manual").unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b-clone");
    git_repo(&a, "git@github.com:favcrm/cadence.git");
    git_repo(&b, "https://github.com/favcrm/cadence");

    let err = restore(
        Path::new(taken["manifest"].as_str().unwrap()),
        &root.path().join("target"),
        &RestoreOptions {
            force: false,
            repos: vec![a, b],
        },
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("github.com/favcrm/cadence"), "{err}");
}

#[test]
fn cad314_remote_urls_normalize_and_lose_credentials() {
    let pw = noise("url-pw", 10);
    assert_eq!(
        strip_credentials(&format!("https://user:{pw}@github.com/o/r.git")),
        "https://github.com/o/r.git"
    );
    assert_eq!(
        strip_credentials("git@github.com:o/r.git"),
        "git@github.com:o/r.git"
    );
    let key = remote_key("https://github.com/o/r.git");
    assert_eq!(key, "github.com/o/r");
    for same in [
        "git@github.com:o/r.git",
        "ssh://git@github.com:22/o/r",
        "https://GitHub.com/O/R/",
        &format!("https://x:{pw}@github.com/o/r.git"),
    ] {
        assert_eq!(remote_key(same), key, "{same}");
    }
    assert_ne!(remote_key("https://github.com/o/other.git"), key);
}

#[test]
fn cad314_self_update_hook_takes_a_backup_first() {
    let root = TempDir::new().unwrap();
    let empty = root.path().join("no-db-yet");
    std::fs::create_dir_all(&empty).unwrap();
    let out = before_self_update(&empty).unwrap();
    assert_eq!(out["skipped"], "no database", "{out}");

    let state = fresh_state(root.path(), "state");
    let out = before_self_update(&state).unwrap();

    let manifest = PathBuf::from(out["manifest"].as_str().unwrap());
    assert_eq!(manifest.parent(), Some(default_dir(&state).as_path()));
    assert_eq!(read_json(&manifest)["reason"], "pre-update");
}

// ---- CAD-396: retention never deletes the fresh copy or foreign files ----

/// Re-home a real backup pair under `cadence-<reason>-<stamp>-<id>` with
/// its manifest's `db_file` (and optionally `created_epoch`) rewritten, so
/// sha256/bytes still match. Returns (db, manifest).
fn plant_pair(
    dir: &Path,
    taken: &Value,
    reason: &str,
    stamp: &str,
    id: &str,
    epoch: Option<f64>,
) -> (PathBuf, PathBuf) {
    let stem = format!("cadence-{reason}-{stamp}-{id}");
    let db = dir.join(format!("{stem}.sqlite3"));
    let manifest = dir.join(format!("{stem}.manifest.json"));
    std::fs::copy(taken["db"].as_str().unwrap(), &db).unwrap();
    let mut m = read_json(Path::new(taken["manifest"].as_str().unwrap()));
    m["db_file"] = json!(format!("{stem}.sqlite3"));
    m["reason"] = json!(reason);
    if let Some(epoch) = epoch {
        m["created_epoch"] = json!(epoch);
    }
    std::fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
    (db, manifest)
}

fn tomorrow_stamp(offset_secs: i64) -> String {
    crate::issue::time::basic(crate::issue::time::now_epoch() + 86_400 + offset_secs)
}

#[test]
fn cad396_planted_future_dated_manifest_never_prunes_the_fresh_copy() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let seed_dir = root.path().join("seed");
    let seed = backup(&state, &seed_dir, 7, "manual").unwrap();
    let dir = root.path().join("b");
    std::fs::create_dir_all(&dir).unwrap();
    let (old_db, old_m) = plant_pair(
        &dir,
        &seed,
        "manual",
        "20991231T235959Z",
        "cafecafe",
        Some(4.0e12),
    );

    let out = backup(&state, &dir, 1, "manual").unwrap();

    let db = PathBuf::from(out["db"].as_str().unwrap());
    let manifest = PathBuf::from(out["manifest"].as_str().unwrap());
    assert!(
        db.is_file() && manifest.is_file(),
        "fresh pair pruned: {out}"
    );
    verify(&manifest).unwrap();
    // keep 1 = the fresh copy only: the planted (matching) pair goes.
    assert!(!old_db.exists() && !old_m.exists(), "{out}");
}

#[test]
fn cad396_clock_skew_before_self_update_keeps_its_fresh_backup() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let seed = backup(&state, &root.path().join("seed"), 7, "manual").unwrap();
    let dir = default_dir(&state);
    std::fs::create_dir_all(&dir).unwrap();
    // Seven honest pre-update backups, dated a day ahead (clock skew).
    let mut planted = Vec::new();
    for i in 0..7 {
        planted.push(plant_pair(
            &dir,
            &seed,
            PRE_UPDATE,
            &tomorrow_stamp(i),
            &format!("{i:08x}"),
            Some(epoch_now() + 86_400.0 + i as f64),
        ));
    }

    let out = before_self_update(&state).unwrap();

    let db = PathBuf::from(out["db"].as_str().unwrap());
    let manifest = PathBuf::from(out["manifest"].as_str().unwrap());
    assert!(db.is_file() && manifest.is_file(), "{out}");
    verify(&manifest).unwrap();
    let kept = manifests(&dir)
        .into_iter()
        .filter(|m| m["reason"] == PRE_UPDATE)
        .count();
    assert_eq!(kept, DEFAULT_KEEP, "{out}");
    // The oldest planted pair (smallest stamp) went, the newest stayed.
    assert!(!planted[0].0.exists() && !planted[0].1.exists());
    assert!(planted[6].0.exists() && planted[6].1.exists());
}

#[test]
fn cad396_old_manifest_naming_a_hand_made_copy_never_deletes_it() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let seed = backup(&state, &root.path().join("seed"), 7, "manual").unwrap();
    let dir = root.path().join("b");
    std::fs::create_dir_all(&dir).unwrap();
    // A hand-made copy with the exact bytes, and an old manifest of ours
    // (right name shape) whose db_file points at it.
    let hand = dir.join("cadence-live-20260101.sqlite3");
    std::fs::copy(seed["db"].as_str().unwrap(), &hand).unwrap();
    let stem = "cadence-manual-20200101T000000Z-aaaaaaaa";
    let mut m = read_json(Path::new(seed["manifest"].as_str().unwrap()));
    m["db_file"] = json!("cadence-live-20260101.sqlite3");
    let old_manifest = dir.join(format!("{stem}.manifest.json"));
    std::fs::write(&old_manifest, serde_json::to_vec(&m).unwrap()).unwrap();

    backup(&state, &dir, 1, "manual").unwrap();

    assert!(hand.exists(), "a hand-made copy is never pruned");
    assert!(
        old_manifest.exists(),
        "a manifest not naming its own stem is not ours"
    );
}

#[test]
fn cad396_prune_skips_a_copy_that_is_a_symlink_or_does_not_match() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let seed = backup(&state, &root.path().join("seed"), 7, "manual").unwrap();
    let dir = root.path().join("b");
    std::fs::create_dir_all(&dir).unwrap();
    // Changed bytes under a matching name.
    let (changed, changed_m) =
        plant_pair(&dir, &seed, "manual", "20200101T000000Z", "11111111", None);
    std::fs::write(&changed, b"operator edited this").unwrap();
    // A symlink to a file outside the dir.
    let (linked, linked_m) =
        plant_pair(&dir, &seed, "manual", "20200101T000001Z", "22222222", None);
    let outside = root.path().join("precious.sqlite3");
    std::fs::rename(&linked, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &linked).unwrap();

    let out = backup(&state, &dir, 1, "manual").unwrap();

    assert!(changed.exists() && changed_m.exists(), "{out}");
    assert!(linked_m.exists() && outside.exists(), "{out}");
    assert!(std::fs::symlink_metadata(&linked).is_ok());
    assert_eq!(out["prune_skipped"].as_array().unwrap().len(), 2, "{out}");
}

#[test]
fn cad396_export_nulls_turn_tokens() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let conn = writer(&state);
    add_agent(&conn, "w1", "/nowhere");
    add_message(&conn, "m1", "w1", "hello");
    conn.execute(
        "UPDATE messages SET state='running', turn_id='turn-abc123' WHERE id='m1'",
        [],
    )
    .unwrap();
    drop(conn);
    let out = export(&state, &root.path().join("bundle")).unwrap();
    let db = root.path().join("bundle").join(BUNDLE_DB);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM messages WHERE turn_id IS NOT NULL"
        ),
        0
    );
    assert!(out["scrubbed"]
        .as_array()
        .unwrap()
        .contains(&json!("messages.turn_id")));
    assert!(!contains(&std::fs::read(&db).unwrap(), "turn-abc123"));
}

#[test]
fn cad396_restore_refuses_a_symlinked_lock() {
    let root = TempDir::new().unwrap();
    let source = fresh_state(root.path(), "source");
    let taken = backup(&source, &root.path().join("b"), DEFAULT_KEEP, "manual").unwrap();
    let target = root.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let elsewhere = root.path().join("elsewhere.lock");
    std::os::unix::fs::symlink(&elsewhere, target.join("cadence.lock")).unwrap();
    let err = restore(
        Path::new(taken["manifest"].as_str().unwrap()),
        &target,
        &RestoreOptions::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("symlink"), "{err}");
    assert!(!elsewhere.exists());
    assert!(!live(&target).exists());
}

#[test]
fn cad396_install_no_clobber_puts_the_old_store_back_on_failure() {
    let root = TempDir::new().unwrap();
    let dir = root.path();
    let live_db = dir.join("cadence.sqlite3");
    let wal = dir.join("cadence.sqlite3-wal");
    std::fs::write(&live_db, b"old store").unwrap();
    std::fs::write(&wal, b"old wal").unwrap();
    // The partial does not exist: hard_link fails after the old files
    // were moved aside.
    let err = install_no_clobber(&dir.join("missing.partial"), &live_db, &[&live_db, &wal])
        .unwrap_err()
        .to_string();
    assert!(err.contains("put back"), "{err}");
    assert_eq!(std::fs::read(&live_db).unwrap(), b"old store");
    assert_eq!(std::fs::read(&wal).unwrap(), b"old wal");
    assert_eq!(
        std::fs::read_dir(dir).unwrap().count(),
        2,
        "no aside files left"
    );

    // Success: new store in place, old store and sidecar gone.
    let partial = dir.join("new.partial");
    std::fs::write(&partial, b"new store").unwrap();
    install_no_clobber(&partial, &live_db, &[&live_db, &wal]).unwrap();
    assert_eq!(std::fs::read(&live_db).unwrap(), b"new store");
    assert!(!wal.exists() && !partial.exists());
    assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1);
}
