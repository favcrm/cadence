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

// ---- CAD-396 review round 2 ----

#[test]
fn cad396_export_redacts_turn_tokens_written_by_mark_running() {
    let root = TempDir::new().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let generation = uuid::Uuid::new_v4().simple().to_string();
    let token = format!("claude-{generation}-{}", uuid::Uuid::new_v4().simple());
    let orphan = format!("pty-{generation}-{}", uuid::Uuid::new_v4().simple());
    {
        drop(Store::open(&live(&state)).unwrap());
        let conn = writer(&state);
        add_agent(&conn, "w1", "/nowhere");
        conn.execute(
            "UPDATE agents SET generation=?1 WHERE alias='w1'",
            [&generation],
        )
        .unwrap();
        add_message(&conn, "m1", "w1", "hello");
        add_message(&conn, "m2", "w1", "doomed");
        // Prose quoting the token, as an operator note would.
        add_message(
            &conn,
            "m3",
            "w1",
            &format!("stale: --token {token} earlier"),
        );
        drop(conn);
        let store = Store::open(&live(&state)).unwrap();
        store.mark_running("m1", &token).unwrap();
        // An event whose message row is gone still carries its token.
        store.mark_running("m2", &orphan).unwrap();
        drop(store);
        writer(&state)
            .execute("DELETE FROM messages WHERE id='m2'", [])
            .unwrap();
    }
    assert!(
        count(
            &live(&state),
            &format!("SELECT count(*) FROM events WHERE payload LIKE '%{token}%'")
        ) > 0,
        "mark_running must have written the token into an event"
    );

    let out = export(&state, &root.path().join("bundle")).unwrap();

    let bytes = std::fs::read(root.path().join("bundle").join(BUNDLE_DB)).unwrap();
    for secret in [&token, &orphan, &generation] {
        assert!(!contains(&bytes, secret), "{secret} left in the bundle");
    }
    assert!(out["redacted"]["cells"].as_u64().unwrap() >= 3, "{out}");
    let db = root.path().join("bundle").join(BUNDLE_DB);
    assert!(
        count(
            &db,
            "SELECT count(*) FROM events WHERE payload LIKE '%[redacted]%'"
        ) >= 2
    );
}

#[test]
fn cad396_remaining_tokens_refuse_the_export() {
    let root = TempDir::new().unwrap();
    let db = root.path().join("x.sqlite3");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE t(v TEXT); INSERT INTO t VALUES ('has 0123456789ab here');
         CREATE TABLE u(v TEXT); INSERT INTO u VALUES ('fine');",
    )
    .unwrap();
    drop(conn);
    let matcher = aho_corasick::AhoCorasick::new(["0123456789ab"]).unwrap();
    let err = refuse_remaining_tokens(&db, &token_shape(), Some(&matcher))
        .unwrap_err()
        .to_string();
    assert!(err.contains("t.v rowid 1"), "{err}");
    assert!(!err.contains("u.v"), "{err}");
}

// ---- CAD-407 ----

fn hex(n: usize) -> String {
    uuid::Uuid::new_v4().simple().to_string()[..n].to_string()
}

fn raw_event(conn: &Connection, payload: &str) {
    conn.execute(
        "INSERT INTO events(alias,kind,payload,at) VALUES('w1','note',?1,1)",
        [payload],
    )
    .unwrap();
}

/// Probe E4: a `turn_id` key whose value is prose is not a token. The
/// old export collected "workspace" from it and rewrote every cell that
/// contained the word — `agents.cwd` and `agents.sandbox` included.
#[test]
fn cad407_prose_under_a_turn_id_key_is_not_redacted() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let conn = writer(&state);
    add_agent(&conn, "w1", "/srv/workspace/repo");
    add_message(&conn, "m1", "w1", r#"{"turn_id": "workspace"}"#);
    raw_event(&conn, r#"{"turn_id":"not-a-token-at-all"}"#);
    drop(conn);

    let out = export(&state, &root.path().join("bundle")).unwrap();

    let db = root.path().join("bundle").join(BUNDLE_DB);
    assert_eq!(
        text(&db, "SELECT cwd FROM agents").as_deref(),
        Some("/srv/workspace/repo")
    );
    assert_eq!(
        text(&db, "SELECT sandbox FROM agents").as_deref(),
        Some("workspace-write")
    );
    assert_eq!(
        text(&db, "SELECT body FROM messages").as_deref(),
        Some(r#"{"turn_id": "workspace"}"#)
    );
    assert_eq!(out["redacted"], json!({"tokens": 0, "cells": 0}), "{out}");
}

/// Probe E2: a token no column and no `"turn_id"` key names — under
/// another key, inside double-encoded JSON, in prose, in any table — is
/// redacted, and so is its generation wherever it stands alone.
#[test]
fn cad407_orphan_tokens_are_redacted_whatever_the_key_or_escaping() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let (g_pty, g_claude) = (hex(32), hex(12));
    let under_other_key = format!("pty-{g_pty}-{}", hex(32));
    let double_encoded = format!("claude-{g_claude}-{}", hex(32));
    let in_prose = format!("pty-{}-{}", hex(32), hex(32));
    let conn = writer(&state);
    add_agent(&conn, "w1", "/nowhere");
    add_message(
        &conn,
        "m1",
        "w1",
        &format!(r#"{{"token":"{under_other_key}"}}"#),
    );
    raw_event(
        &conn,
        &format!(r#"{{"inner":"{{\"turn_id\":\"{double_encoded}\"}}"}}"#),
    );
    add_message(
        &conn,
        "m2",
        "w1",
        &format!("reported with --token {in_prose}; pane generation {g_pty} was live"),
    );
    raw_event(&conn, &format!(r#"{{"generation":"{g_claude}"}}"#));
    drop(conn);

    let out = export(&state, &root.path().join("bundle")).unwrap();

    let db = root.path().join("bundle").join(BUNDLE_DB);
    let bytes = std::fs::read(&db).unwrap();
    for secret in [
        &under_other_key,
        &double_encoded,
        &in_prose,
        &g_pty,
        &g_claude,
    ] {
        assert!(!contains(&bytes, secret), "{secret} left in the bundle");
    }
    assert_eq!(out["redacted"]["cells"], json!(4), "{out}");
    assert_eq!(
        text(&db, "SELECT body FROM messages WHERE id='m2'").as_deref(),
        Some("reported with --token [redacted]; pane generation [redacted] was live")
    );
    assert_eq!(
        text(&db, "SELECT body FROM messages WHERE id='m1'").as_deref(),
        Some(r#"{"token":"[redacted]"}"#)
    );
}

/// With a store in place as well, the refusal cannot tell a finished
/// restore from an empty store created after the interruption: it names
/// both recoveries, each with its commands, and every sidecar's own name.
#[test]
fn cad407_leftover_refusal_names_both_recoveries_when_a_store_exists() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let db = state.join("cadence.sqlite3.replaced-20260923T000000Z-deadbeef");
    let wal = state.join("cadence.sqlite3-wal.replaced-20260923T000000Z-deadbeef");
    std::fs::write(&db, b"previous store").unwrap();
    std::fs::write(&wal, b"previous wal").unwrap();

    let err = refuse_interrupted_restore(&state).unwrap_err().to_string();

    // Listed in name order: "-wal" sorts before ".replaced".
    let q = |p: &Path| format!("'{}'", p.display());
    assert!(err.contains("If it is the store you restored"), "{err}");
    assert!(
        err.contains(&format!(
            "mv {} {} {}",
            q(&wal),
            q(&db),
            q(&default_dir(&state))
        )),
        "{err}"
    );
    assert!(
        err.contains(&format!(
            "mv {} {} && mv {} {}",
            q(&wal),
            q(&state.join("cadence.sqlite3-wal")),
            q(&db),
            q(&live(&state))
        )),
        "{err}"
    );
    assert!(live(&state).exists() && db.exists() && wal.exists());
}

/// A token spelled with JSON `\u` escapes cannot be redacted in place;
/// the recheck reads through the escapes and refuses the export.
#[test]
fn cad407_an_escaped_token_refuses_the_export() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let conn = writer(&state);
    add_agent(&conn, "w1", "/nowhere");
    // `-` as a JSON unicode escape: backslash, "u002d".
    let dash = ['\\'.to_string(), "u002d".to_string()].concat();
    raw_event(
        &conn,
        &format!(r#"{{"t":"pty{dash}{}{dash}{}"}}"#, hex(32), hex(32)),
    );
    drop(conn);
    let out = root.path().join("bundle");
    let err = export(&state, &out).unwrap_err().to_string();
    assert!(err.contains("events.payload rowid"), "{err}");
    assert!(!out.exists());
}

// ---- CAD-424 ----

/// An old endpoint generation logged alone — the `ready` or `pane_root`
/// event of an endpoint since replaced, with no surviving token and no
/// agent row naming it — is redacted wherever it appears, at any JSON
/// escape depth. Hex under other keys, and a `"generation"` value that is
/// not generation-shaped, stay.
#[test]
fn cad424_a_generation_logged_alone_is_redacted() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let (ready, pane_root, nested) = (hex(32), hex(12), hex(32));
    let (message_id, owner) = (hex(32), hex(32));
    let conn = writer(&state);
    add_agent(&conn, "w1", "/nowhere");
    raw_event(
        &conn,
        &format!(r#"{{"thread_id":null,"pid":7,"endpoint":"pty","generation":"{ready}"}}"#),
    );
    raw_event(
        &conn,
        &format!(r#"{{"pid":7,"start_time":1,"sid":7,"generation":"{pane_root}"}}"#),
    );
    raw_event(
        &conn,
        &format!(r#"{{"inner":"{{\"generation\":\"{nested}\"}}"}}"#),
    );
    add_message(
        &conn,
        "m1",
        "w1",
        &format!("pane reopened; generation {ready} is gone"),
    );
    let kept = format!(
        r#"{{"message":"{message_id}","owner_generation":"{owner}","generation":"gen-1"}}"#
    );
    raw_event(&conn, &kept);
    drop(conn);

    let out = export(&state, &root.path().join("bundle")).unwrap();

    let db = root.path().join("bundle").join(BUNDLE_DB);
    let bytes = std::fs::read(&db).unwrap();
    for generation in [&ready, &pane_root, &nested] {
        assert!(
            !contains(&bytes, generation),
            "{generation} left in the bundle"
        );
    }
    assert_eq!(
        text(&db, "SELECT body FROM messages WHERE id='m1'").as_deref(),
        Some("pane reopened; generation [redacted] is gone")
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT count(*) FROM events WHERE payload = '{kept}'")
        ),
        1,
        "hex under other keys and a non-generation value must be left alone"
    );
    assert_eq!(out["redacted"], json!({"tokens": 3, "cells": 4}), "{out}");
}

/// A logged generation spelled with JSON `\u` escapes cannot be redacted
/// in place; the recheck reads through the escapes and refuses.
#[test]
fn cad424_an_escaped_logged_generation_refuses_the_export() {
    let root = TempDir::new().unwrap();
    let state = fresh_state(root.path(), "state");
    let conn = writer(&state);
    add_agent(&conn, "w1", "/nowhere");
    // "a" as a JSON unicode escape: backslash, "u0061".
    let a = ['\\'.to_string(), "u0061".to_string()].concat();
    raw_event(&conn, &format!(r#"{{"generation":"{a}{}"}}"#, hex(11)));
    drop(conn);
    let out = root.path().join("bundle");
    let err = export(&state, &out).unwrap_err().to_string();
    assert!(err.contains("events.payload rowid"), "{err}");
    assert!(!out.exists());
}

#[test]
fn cad396_restore_refuses_after_an_interrupted_forced_restore() {
    let root = TempDir::new().unwrap();
    let source = fresh_state(root.path(), "source");
    let taken = backup(&source, &root.path().join("b"), DEFAULT_KEEP, "manual").unwrap();
    let target = root.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    // What a crash between "rename aside" and "link in" leaves behind.
    let aside = target.join("cadence.sqlite3.replaced-20260923T000000Z-deadbeef");
    std::fs::write(&aside, b"previous store").unwrap();
    assert_eq!(interrupted_restore_leftovers(&target), vec![aside.clone()]);
    for force in [false, true] {
        let err = restore(
            Path::new(taken["manifest"].as_str().unwrap()),
            &target,
            &RestoreOptions {
                force,
                repos: vec![],
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("interrupted"), "{err}");
    }
    assert!(!live(&target).exists());
    assert_eq!(std::fs::read(&aside).unwrap(), b"previous store");
}

#[test]
fn cad396_a_failed_rollback_is_reported_not_hidden() {
    let root = TempDir::new().unwrap();
    let from = root.path().join("cadence.sqlite3");
    let aside = root.path().join("cadence.sqlite3.replaced-x");
    // The aside file is gone: renaming it back fails.
    let err = put_back(&[(from.clone(), aside.clone())], "could not install".into()).to_string();
    assert!(err.contains("ROLLBACK FAILED"), "{err}");
    assert!(!err.contains("was put back"), "{err}");
    assert!(err.contains(&aside.display().to_string()), "{err}");
}

/// CAD-319: the thread tables ride the generic export scrub. Entries the
/// store wrote are already redacted, so they never refuse an export; a
/// turn token quoted in thread prose is redacted like any other text
/// cell; and a credential planted into `thread_entries` behind the
/// store's back still refuses the export, naming the column.
#[test]
fn cad319_export_scans_and_redacts_thread_entries() {
    let root = TempDir::new().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let pat = github_token("thread-export");
    let generation = uuid::Uuid::new_v4().simple().to_string();
    let token = format!("claude-{generation}-{}", uuid::Uuid::new_v4().simple());
    {
        drop(Store::open(&live(&state)).unwrap());
        let conn = writer(&state);
        add_agent(&conn, "master", "/nowhere");
        conn.execute(
            "UPDATE agents SET generation=?1 WHERE alias='master'",
            [&generation],
        )
        .unwrap();
        drop(conn);
        let store = Store::open(&live(&state)).unwrap();
        store.ensure_thread("master").unwrap();
        store
            .enqueue("master", "please do the thing", None, "m1", "user")
            .unwrap();
        store.mark_running("m1", &token).unwrap();
        store
            .thread_append_running(
                "master",
                crate::store::ROLE_AGENT,
                crate::store::KIND_ASSISTANT_TEXT,
                &format!("my token is {token} and a key {pat}"),
                None,
            )
            .unwrap();
    }
    assert_eq!(
        count(
            &live(&state),
            &format!("SELECT count(*) FROM thread_entries WHERE text LIKE '%{pat}%'")
        ),
        0,
        "the store redacts before writing"
    );

    let out = export(&state, &root.path().join("bundle")).unwrap();

    let db = root.path().join("bundle").join(BUNDLE_DB);
    let bytes = std::fs::read(&db).unwrap();
    for secret in [&token, &generation, &pat] {
        assert!(!contains(&bytes, secret), "{secret} left in the bundle");
    }
    assert!(
        count(
            &db,
            "SELECT count(*) FROM thread_entries WHERE text LIKE '%[redacted]%'"
        ) == 1,
        "{out}"
    );
    assert_eq!(count(&db, "SELECT count(*) FROM thread_entries"), 2);

    // Planted raw — the export scan covers the new table like any other.
    let conn = writer(&state);
    conn.execute(
        "UPDATE thread_entries SET payload=?1 WHERE seq=(SELECT max(seq) FROM thread_entries)",
        [format!("{{\"raw\":\"{pat}\"}}")],
    )
    .unwrap();
    drop(conn);
    let out_dir = root.path().join("bundle2");
    let err = export(&state, &out_dir).unwrap_err().to_string();
    assert!(err.contains("thread_entries.payload"), "{err}");
    assert!(!err.contains(&pat), "{err}");
    assert!(!out_dir.exists());
}

/// CAD-410: a private key with no END marker (a head-limited read quoted
/// in thread prose) is redacted when the store writes it, so none of its
/// body reaches the export bundle — the export's own scan only flags a
/// whole key.
#[test]
fn cad410_export_carries_no_body_of_a_truncated_private_key() {
    let root = TempDir::new().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let body: Vec<String> = (0..8)
        .map(|i| noise(&format!("cad410-export:{i}"), 64))
        .collect();
    let key = format!(
        "{}\n{}",
        ["-----BEGIN ", "PRIVATE", " KEY-----"].concat(),
        body.join("\n")
    );
    {
        drop(Store::open(&live(&state)).unwrap());
        let conn = writer(&state);
        add_agent(&conn, "master", "/nowhere");
        drop(conn);
        let store = Store::open(&live(&state)).unwrap();
        store.ensure_thread("master").unwrap();
        store
            .enqueue("master", "show me the key", None, "m1", "user")
            .unwrap();
        store.mark_running("m1", "tok-cad410").unwrap();
        store
            .thread_append_running(
                "master",
                crate::store::ROLE_AGENT,
                crate::store::KIND_ASSISTANT_TEXT,
                &format!("head -n 9 of the key file:\n{key}"),
                Some(serde_json::json!({"quoted": key})),
            )
            .unwrap();
    }

    export(&state, &root.path().join("bundle")).unwrap();

    let bytes = std::fs::read(root.path().join("bundle").join(BUNDLE_DB)).unwrap();
    for line in &body {
        assert!(!contains(&bytes, line), "key body left in the bundle");
    }
    assert!(contains(&bytes, "[redacted:private-key]"));
}
