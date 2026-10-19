//! CAD-1163 independent acceptance: portable coordination export refusals and
//! positives. Author: cad1163-devin-haiku-acceptance (independent of the
//! implementer). Written from the ticket and
//! /tmp/renewal-release-prep/cad1163-resolved-policy.md, not from producer
//! code. The implementer must not edit or weaken these checks.
//!
//! Exercises the real product entrypoints `export_portable` and
//! `refuse_if_inactive`. Restore and activation have no executable interface
//! yet (`restore_inactive` is a stub, `NativeOperatorProof` has no public
//! constructor), so no restore or activation pass is claimed here.
//!
//! Every fixture lives under /tmp/cad1163-export-acceptance/. No production
//! state, credentials or ports are touched. Token-shaped strings are built at
//! runtime, as in backup/tests.rs.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{params, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::backup::portable::{
    export_portable, refuse_if_inactive, restore_inactive, PortableExportOptions,
    PortableRestoreOptions, Scope, INACTIVE_MARKER,
};
use crate::rollout::db_file;
use crate::store::Store;

mod history_sanitization_acceptance;

const ROOT: &str = "/tmp/cad1163-export-acceptance";
const TICKET_REL: &str = "tickets/CAD-1163.md";
const TICKET_BODY: &str = "# CAD-1163 synthetic ticket fixture\n\nPortable export acceptance.\n";
const SOURCE_UUID: &str = "synthetic-source-workspace-1163";

/// Synthetic markers that must never appear in a coordination package:
/// unselected credential, grant, model, daemon and live-authority values.
const FORBIDDEN_MARKERS: &[&str] = &[
    "SYNTH-SCOPE-CRED-1163",
    "SYNTH-CUSTODY-REF-1163",
    "SYNTH-SCOPE-GRANT-1163",
    "SYNTH-APP-GRANT-1163",
    "SYNTH-INSTALL-1163",
    "SYNTH-MODEL-DEFAULT-1163",
    "SYNTH-COMMIT-1163",
    "PARAM-LIVE-1155",
    "gen-LIVE-1155",
    "sess-LIVE-1155",
    "thr-LIVE-1155",
    "turn-LIVE-1155-A",
    "unix:/tmp/cad1163-live.sock",
];

/// Tables whose unselected rows must be excluded from a coordination package.
const EXCLUDED_TABLES: &[&str] = &[
    "platform_credentials",
    "platform_grants",
    "app_grants",
    "model_defaults",
    "daemon_build",
];

fn root(name: &str) -> PathBuf {
    let dir = Path::new(ROOT).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
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
            // Git 2.55 spawns detached post-commit background maintenance that
            // races byte-for-byte snapshot assertions on fixture trees.
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "cad1163-acceptance")
        .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
        .env("GIT_COMMITTER_NAME", "cad1163-acceptance")
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

/// A clean tracker: one commit holding the ticket file.
fn make_tracker(root: &Path) -> PathBuf {
    let tracker = root.join("tracker");
    fs::create_dir_all(tracker.join("tickets")).unwrap();
    git(&tracker, &["init", "-q"]);
    fs::write(tracker.join(TICKET_REL), TICKET_BODY).unwrap();
    git(&tracker, &["add", "-A"]);
    git(&tracker, &["commit", "-q", "-m", "CAD-1163 fixture"]);
    tracker
}

/// A current-schema store with coordination rows. `seed_unselected` adds
/// synthetic credential, grant, model and daemon rows that the coordination
/// profile must exclude.
fn make_state(root: &Path, seed_unselected: bool) -> PathBuf {
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    drop(Store::open(&db_file(&state)).unwrap());
    let conn = Connection::open(db_file(&state)).unwrap();
    conn.execute(
        "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,
                            generation,pid,pid_start,endpoint,session_id,thread_id,params,
                            enabled,created,updated)
         VALUES('cad-worker','codex','stdio','worker','/home/ubuntu/Project/synthetic',
                'workspace-write','idle','gen-LIVE-1155',4242,99,'unix:/tmp/cad1163-live.sock',
                'sess-LIVE-1155','thr-LIVE-1155','{\"synthetic\":\"PARAM-LIVE-1155\"}',1,1,1)",
        [],
    )
    .unwrap();
    let bodies = [
        "ticket CAD-1163 decision recorded".to_string(),
        "handoff for CAD-1163 review".to_string(),
        "review complete for CAD-1163".to_string(),
    ];
    for (i, body) in bodies.iter().enumerate() {
        conn.execute(
            "INSERT INTO messages(id,alias,body,source,state,created,turn_id)
             VALUES(?1,'cad-worker',?2,'cli','completed',?3,'turn-LIVE-1155-A')",
            params![format!("msg-000{}", i + 1), body, (i + 1) as i64],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT OR REPLACE INTO connection_metadata(singleton, workspace_id) VALUES(1, ?1)",
        params![SOURCE_UUID],
    )
    .unwrap();
    if seed_unselected {
        conn.execute(
            "INSERT INTO platform_credentials(platform,account,scopes,fingerprint,custody,exchange,enrolled_at,by)
             VALUES('synthetic-platform','acct-1163','SYNTH-SCOPE-CRED-1163','fp-1163',
                    'SYNTH-CUSTODY-REF-1163','exch-1163',1,'synthetic-operator')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO platform_grants(agent,platform,account,scopes,granted_at,by)
             VALUES('cad-worker','synthetic-platform','acct-1163','SYNTH-SCOPE-GRANT-1163',1,'synthetic-operator')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO app_grants(app,agent,platform,account,scopes,granted_at,by,install_id)
             VALUES('synthetic-app','cad-worker','synthetic-platform','acct-1163',
                    'SYNTH-APP-GRANT-1163',1,'synthetic-operator','SYNTH-INSTALL-1163')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE model_defaults SET document=?1 WHERE id=1",
            params!["{\"schema\":1,\"providers\":{\"synthetic\":\"SYNTH-MODEL-DEFAULT-1163\"}}"],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO daemon_build(id,commit_sha,recorded_at) VALUES(1,'SYNTH-COMMIT-1163',1)",
            [],
        )
        .unwrap();
    }
    drop(conn);
    state
}

fn opts(tracker: &Path) -> PortableExportOptions {
    PortableExportOptions {
        scope: Scope::Coordination,
        tracker: tracker.to_path_buf(),
        redact_history_secrets: false,
    }
}

fn read_manifest(out: &Path) -> Value {
    serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap()
}

fn sha_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Every regular file under `dir` with its bytes, keyed by relative path.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else if let Ok(bytes) = fs::read(&p) {
                out.insert(
                    p.strip_prefix(base).unwrap().to_string_lossy().into_owned(),
                    bytes,
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn count(db: &Path, table: &str) -> i64 {
    Connection::open(db)
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// No `.<name>.partial-*` staging entry is left next to `out`.
fn no_partial(out: &Path) -> bool {
    let parent = out.parent().unwrap();
    let name = out.file_name().unwrap().to_string_lossy().into_owned();
    let prefix = format!(".{name}.partial-");
    fs::read_dir(parent)
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with(&prefix))
}

fn package_bytes(out: &Path) -> Vec<u8> {
    tree(out).values().flatten().copied().collect()
}

/// Shared positive checks for a clean-source export.
fn assert_clean_package(out: &Path, state: &Path, tracker: &Path, head_before: &str) {
    let manifest = read_manifest(out);
    assert_eq!(manifest["format"], "cadence.portable/1");
    assert_eq!(manifest["scope"], "coordination");
    assert_eq!(
        manifest["full_workspace"], false,
        "coordination is not a full workspace"
    );
    assert_eq!(
        manifest["tracker_revision"], head_before,
        "tracker revision pinned"
    );
    assert_eq!(
        manifest["source_uuid"], SOURCE_UUID,
        "source UUID is provenance"
    );

    // Every member matches the bytes on disk.
    let members = manifest["members"].as_array().unwrap();
    let paths: Vec<&str> = members
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"cadence.sqlite3"),
        "db member present: {paths:?}"
    );
    assert!(
        paths.contains(&format!("tracker/{TICKET_REL}").as_str()),
        "ticket member present: {paths:?}"
    );
    for m in members {
        let file = out.join(m["path"].as_str().unwrap());
        let bytes = fs::read(&file).unwrap();
        assert_eq!(m["sha256"], sha_hex(&bytes), "digest of {}", file.display());
        assert_eq!(m["bytes"], bytes.len() as u64);
    }
    // Ticket content survives byte-for-byte.
    assert_eq!(
        fs::read(out.join("tracker").join(TICKET_REL)).unwrap(),
        TICKET_BODY.as_bytes()
    );
    assert!(
        !out.join("tracker/.git").exists() && !out.join(".git").exists(),
        "no .git copied"
    );

    // Historical message IDs, order and bodies survive; authority is cleared.
    let db = out.join("cadence.sqlite3");
    let conn = Connection::open(&db).unwrap();
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM messages ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        ids,
        ["msg-0001", "msg-0002", "msg-0003"],
        "message IDs and order"
    );
    let turn_null: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages WHERE turn_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(turn_null, 0, "turn tokens cleared in the package");
    let (gen, pid, endpoint, enabled): (Option<String>, Option<i64>, Option<String>, i64) = conn
        .query_row(
            "SELECT generation, pid, endpoint, enabled FROM agents WHERE alias='cad-worker'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert!(
        gen.is_none() && pid.is_none() && endpoint.is_none(),
        "live authority nulled"
    );
    assert_eq!(enabled, 0, "imported actor never enabled");
    assert_eq!(
        count(&db, "connection_metadata"),
        0,
        "source UUID is not an active replica"
    );

    // Forbidden synthetic authority values never reach the package.
    let blob = package_bytes(out);
    for marker in FORBIDDEN_MARKERS {
        assert!(!contains(&blob, marker), "package carries {marker}");
    }
    assert!(!contains(
        &fs::read(out.join("manifest.json")).unwrap(),
        "SYNTH-"
    ));

    // Source is read only: the live store and tracker are unchanged.
    let src = Connection::open(db_file(state)).unwrap();
    let src_enabled: i64 = src
        .query_row(
            "SELECT enabled FROM agents WHERE alias='cad-worker'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(src_enabled, 1, "source agent left as it was");
    assert_eq!(
        git(
            tracker,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "",
        "tracker still clean"
    );
    assert_eq!(
        git(tracker, &["rev-parse", "HEAD"]),
        head_before,
        "tracker HEAD unchanged"
    );
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

#[test]
fn cad1163_acc_export_clean_store_publishes_coordination_package() {
    let root = root("clean");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let head = git(&tracker, &["rev-parse", "HEAD"]);
    let db_before = fs::read(db_file(&state)).unwrap();

    let out = root.join("package");
    let report = export_portable(&state, &out, &opts(&tracker)).expect("clean export succeeds");
    assert_eq!(report["scope"], "coordination");
    assert_eq!(report["full_workspace"], false);
    assert!(
        out.is_dir() && no_partial(&out),
        "published once, no staging left"
    );
    assert_eq!(
        fs::read(db_file(&state)).unwrap(),
        db_before,
        "live store bytes unchanged"
    );
    assert_clean_package(&out, &state, &tracker, &head);
    // Omitted apps are listed, not silently dropped.
    let manifest = read_manifest(&out);
    assert!(manifest["omitted_tables"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "app_runs"));
}

#[test]
fn cad1163_acc_export_excludes_unselected_authority_rows() {
    let root = root("unselected");
    let tracker = make_tracker(&root);
    let state = make_state(&root, true);
    let head = git(&tracker, &["rev-parse", "HEAD"]);

    let out = root.join("package");
    export_portable(&state, &out, &opts(&tracker))
        .expect("unselected credential/grant/model/daemon rows are excluded, not refused");
    assert_clean_package(&out, &state, &tracker, &head);

    let db = out.join("cadence.sqlite3");
    for table in EXCLUDED_TABLES {
        assert_eq!(
            count(&db, table),
            0,
            "{table} rows excluded from the package"
        );
    }
    let manifest = read_manifest(&out);
    let omitted: Vec<&str> = manifest["omitted_tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    for table in EXCLUDED_TABLES {
        assert!(
            omitted.contains(table),
            "{table} disclosed as omitted: {omitted:?}"
        );
    }
    assert!(
        !manifest["exclusions"].as_array().unwrap().is_empty(),
        "exclusions disclosed"
    );
}

#[test]
fn cad1163_acc_export_refuses_unknown_core_authority_column() {
    let root = root("unknown-column");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    {
        let conn = Connection::open(db_file(&state)).unwrap();
        conn.execute("ALTER TABLE messages ADD COLUMN reusable_grant TEXT", [])
            .unwrap();
        conn.execute(
            "UPDATE messages SET reusable_grant='SYNTH-REUSABLE-GRANT-1163' WHERE id='msg-0001'",
            [],
        )
        .unwrap();
    }
    let out = root.join("package");
    let err = export_portable(&state, &out, &opts(&tracker));
    assert!(err.is_err(), "unknown authority-bearing column refuses");
    assert!(
        !out.exists() && no_partial(&out),
        "no output or staging written"
    );
    assert_eq!(
        git(&tracker, &["status", "--porcelain=v1"]),
        "",
        "tracker untouched"
    );
}

/// CAD-1420: the current 47-schema `assistants` table is in the known catalog
/// (positive control, unmodified store exports), and one forged extra column
/// on that table refuses with the exact catalog error and writes nothing.
#[test]
fn cad1420_acc_export_refuses_unknown_assistants_column() {
    let root = root("unknown-assistants-column");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let out = root.join("package-control");
    export_portable(&state, &out, &opts(&tracker))
        .expect("current 47 assistants catalog is supported");
    fs::remove_dir_all(&out).unwrap();
    {
        let conn = Connection::open(db_file(&state)).unwrap();
        conn.execute("ALTER TABLE assistants ADD COLUMN forged_grant TEXT", [])
            .unwrap();
    }
    let out = root.join("package");
    let err = export_portable(&state, &out, &opts(&tracker))
        .expect_err("unknown assistants column refuses");
    assert_eq!(
        err.to_string(),
        "portable export refuses: table assistants columns do not match the known catalog"
    );
    assert!(
        !out.exists() && no_partial(&out),
        "no output or staging written"
    );
    assert_eq!(
        git(&tracker, &["status", "--porcelain=v1"]),
        "",
        "tracker untouched"
    );
}

#[test]
fn cad1163_acc_export_refuses_unknown_app_table_with_rows() {
    let root = root("unknown-app");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    {
        let conn = Connection::open(db_file(&state)).unwrap();
        conn.execute(
            "CREATE TABLE app_unknown_guess(id INTEGER PRIMARY KEY, payload TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO app_unknown_guess(payload) VALUES('SYNTH-UNKNOWN-APP-1163')",
            [],
        )
        .unwrap();
    }
    let out = root.join("package");
    assert!(
        export_portable(&state, &out, &opts(&tracker)).is_err(),
        "unknown app_* table with rows refuses"
    );
    assert!(!out.exists() && no_partial(&out));
}

#[test]
fn cad1163_acc_export_refuses_existing_or_symlinked_output_without_writes() {
    let root = root("collision");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);

    let existing = root.join("existing");
    fs::create_dir_all(&existing).unwrap();
    fs::write(existing.join("keep.txt"), b"operator data").unwrap();
    let before = tree(&existing);
    assert!(export_portable(&state, &existing, &opts(&tracker)).is_err());
    assert_eq!(tree(&existing), before, "existing output byte-identical");

    let elsewhere = root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("keep.txt"), b"other data").unwrap();
    let link = root.join("link");
    symlink(&elsewhere, &link).unwrap();
    let before = tree(&elsewhere);
    assert!(
        export_portable(&state, &link, &opts(&tracker)).is_err(),
        "symlink output refuses"
    );
    assert_eq!(tree(&elsewhere), before, "symlink target untouched");
}

#[test]
fn cad1163_acc_git_admission_refuses_alternates_outside_roots() {
    let root = root("alternates");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let foreign = root.join("foreign");
    fs::create_dir_all(&foreign).unwrap();
    git(&foreign, &["init", "-q"]);
    fs::write(foreign.join("secret.txt"), "SYNTH-FOREIGN-OBJECT-1163").unwrap();
    git(&foreign, &["add", "-A"]);
    git(&foreign, &["commit", "-q", "-m", "foreign"]);
    fs::write(
        tracker.join(".git/objects/info/alternates"),
        foreign.join(".git/objects").to_string_lossy().as_bytes(),
    )
    .unwrap();
    let before = tree(&foreign.join(".git"));

    let out = root.join("package");
    assert!(
        export_portable(&state, &out, &opts(&tracker)).is_err(),
        "alternates outside owned roots refuse"
    );
    assert!(!out.exists() && no_partial(&out));
    assert_eq!(
        tree(&foreign.join(".git")),
        before,
        "foreign repository untouched"
    );
}

#[test]
fn cad1163_acc_git_admission_refuses_symlink_entry() {
    let root = root("symlink-entry");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    symlink("/etc/hostname", tracker.join("tickets/link.md")).unwrap();
    git(&tracker, &["add", "-A"]);
    git(&tracker, &["commit", "-q", "-m", "symlink"]);

    let out = root.join("package");
    assert!(
        export_portable(&state, &out, &opts(&tracker)).is_err(),
        "symlink entry refuses"
    );
    assert!(!out.exists() && no_partial(&out));
}

#[test]
fn cad1163_acc_git_admission_refuses_gitlink_entry() {
    let root = root("gitlink");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let head = git(&tracker, &["rev-parse", "HEAD"]);
    git(
        &tracker,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},sub"),
        ],
    );
    git(&tracker, &["commit", "-q", "-m", "gitlink"]);

    let out = root.join("package");
    assert!(
        export_portable(&state, &out, &opts(&tracker)).is_err(),
        "gitlink entry refuses"
    );
    assert!(!out.exists() && no_partial(&out));
}

#[test]
fn cad1163_acc_git_admission_refuses_hardlink_outside_roots() {
    let root = root("hardlink");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let head = git(&tracker, &["rev-parse", "HEAD"]);
    // The HEAD commit object is a loose file under the tracker's own .git.
    let object = tracker
        .join(".git/objects")
        .join(&head[..2])
        .join(&head[2..]);
    assert!(object.is_file(), "loose HEAD commit object exists");
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::hard_link(&object, outside.join("shared-object")).unwrap();
    let before = fs::read(&object).unwrap();

    let out = root.join("package");
    assert!(
        export_portable(&state, &out, &opts(&tracker)).is_err(),
        "object hardlinked outside owned roots refuses"
    );
    assert!(!out.exists() && no_partial(&out));
    assert_eq!(
        fs::read(&object).unwrap(),
        before,
        "shared object unchanged"
    );
}

#[test]
fn cad1163_acc_export_refuses_dirty_tracker() {
    let root = root("dirty");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    fs::write(tracker.join("tickets/untracked.md"), "not committed\n").unwrap();

    let out = root.join("package");
    assert!(
        export_portable(&state, &out, &opts(&tracker)).is_err(),
        "untracked tracker content refuses"
    );
    assert!(!out.exists() && no_partial(&out));
}

#[test]
fn cad1163_acc_inactive_marker_refuses_daemon_start() {
    let root = root("inactive");
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    assert!(
        refuse_if_inactive(&state).is_ok(),
        "no marker: start allowed"
    );
    fs::write(state.join(INACTIVE_MARKER), "{}").unwrap();
    assert!(
        refuse_if_inactive(&state).is_err(),
        "committed-inactive restore refuses start"
    );
}

// ---------- inactive restore (real roundtrip and refusals) ----------

/// Export a clean fixture package under `root`, returning the package path.
fn export_fixture(root: &Path, tracker: &Path, state: &Path) -> PathBuf {
    let out = root.join("package");
    export_portable(state, &out, &opts(tracker)).expect("fixture export succeeds");
    out
}

fn restore_opts() -> PortableRestoreOptions {
    PortableRestoreOptions { replace: false }
}

/// Recompute member digests and the manifest's own bookkeeping after a
/// mutation, so the only defect left is the one the test plants.
fn rehash_manifest(out: &Path) {
    let mut manifest = read_manifest(out);
    let members: Vec<Value> = manifest["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            let file = out.join(m["path"].as_str().unwrap());
            let bytes = fs::read(&file).unwrap();
            serde_json::json!({"path": m["path"], "bytes": bytes.len(), "sha256": sha_hex(&bytes)})
        })
        .collect();
    manifest["members"] = Value::Array(members);
    fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

/// Mutate the package store and recompute its digests. The planted defect is
/// the only thing a restore can reject.
fn forge_store(out: &Path, sql: &str) {
    Connection::open(out.join("cadence.sqlite3"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
    rehash_manifest(out);
}

fn destination_untouched(dest: &Path) -> bool {
    !dest.exists()
}

#[test]
fn cad1163_acc_restore_roundtrip_commits_inactive_with_selected_data() {
    let root = root("roundtrip");
    let tracker = make_tracker(&root);
    let state = make_state(&root, true);
    let package = export_fixture(&root, &tracker, &state);
    let db_before = fs::read(db_file(&state)).unwrap();

    let dest = root.join("restored-state");
    let report =
        restore_inactive(&package, &dest, &restore_opts()).expect("clean roundtrip restores");
    assert_eq!(report["state"], "committed_inactive");

    // Source is read only.
    assert_eq!(
        fs::read(db_file(&state)).unwrap(),
        db_before,
        "source store unchanged"
    );

    // Selected coordination data survives with message IDs and order.
    let db = dest.join("cadence.sqlite3");
    let conn = Connection::open(&db).unwrap();
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM messages ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(ids, ["msg-0001", "msg-0002", "msg-0003"]);
    let (enabled, gen, pid): (i64, Option<String>, Option<i64>) = conn
        .query_row(
            "SELECT enabled, generation, pid FROM agents WHERE alias='cad-worker'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(enabled, 0, "restore commits with no actor enabled");
    assert!(
        gen.is_none() && pid.is_none(),
        "no live generation or pid after restore"
    );
    assert_eq!(
        count(&db, "platform_credentials"),
        0,
        "unselected credential rows absent"
    );
    assert_eq!(
        count(&db, "platform_grants"),
        0,
        "unselected grant rows absent"
    );
    assert_eq!(count(&db, "app_grants"), 0, "app grants absent");

    // Inactive marker is written; the daemon refuses to start over it.
    assert!(
        dest.join(INACTIVE_MARKER).is_file(),
        "inactive marker committed"
    );
    assert!(
        refuse_if_inactive(&dest).is_err(),
        "daemon start refuses while inactive"
    );

    // Tracker copied: ticket bytes match the source ticket.
    assert_eq!(
        fs::read(dest.join("portable-tracker").join(TICKET_REL)).unwrap(),
        TICKET_BODY.as_bytes(),
        "ticket content restored byte-for-byte"
    );
}

#[test]
fn cad1163_acc_restore_refuses_forged_enabled_agent_with_recomputed_hashes() {
    let root = root("forged-enabled");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let package = export_fixture(&root, &tracker, &state);
    forge_store(
        &package,
        "UPDATE agents SET enabled=1 WHERE alias='cad-worker';",
    );

    let dest = root.join("dest");
    assert!(
        restore_inactive(&package, &dest, &restore_opts()).is_err(),
        "forged enabled actor refused even with recomputed digests"
    );
    assert!(
        destination_untouched(&dest),
        "refused restore writes nothing to destination"
    );
}

#[test]
fn cad1163_acc_restore_refuses_forged_live_authority_with_recomputed_hashes() {
    let root = root("forged-authority");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let package = export_fixture(&root, &tracker, &state);
    forge_store(
        &package,
        "UPDATE agents SET generation='gen-LIVE-1155', pid=4242 WHERE alias='cad-worker';",
    );

    let dest = root.join("dest");
    assert!(
        restore_inactive(&package, &dest, &restore_opts()).is_err(),
        "forged live generation/pid refused with recomputed digests"
    );
    assert!(destination_untouched(&dest));
}

#[test]
fn cad1163_acc_restore_refuses_reinjected_credential_rows_with_recomputed_hashes() {
    let root = root("forged-credential");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let package = export_fixture(&root, &tracker, &state);
    forge_store(
        &package,
        "INSERT INTO platform_credentials(platform,account,scopes,fingerprint,custody,exchange,enrolled_at,by)
         VALUES('synthetic-platform','acct-forged','SYNTH-SCOPE-CRED-1163','fp','SYNTH-CUSTODY-REF-1163','exch',1,'x');",
    );
    let dest = root.join("dest");
    assert!(
        restore_inactive(&package, &dest, &restore_opts()).is_err(),
        "reinjected credential refused"
    );
    assert!(destination_untouched(&dest));
}

#[test]
fn cad1163_acc_restore_refuses_fk_orphan_with_recomputed_hashes() {
    let root = root("fk-orphan");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let package = export_fixture(&root, &tracker, &state);
    // tasks.job_id references jobs(id); no job exists in the fixture.
    forge_store(
        &package,
        "PRAGMA foreign_keys=OFF;
         INSERT INTO tasks(id,job_id,title,role,state,revision,created,updated)
         VALUES('task-orphan','job-missing','orphan','implementer','open',0,1,1);",
    );
    let dest = root.join("dest");
    assert!(
        restore_inactive(&package, &dest, &restore_opts()).is_err(),
        "foreign-key orphan refused by restore"
    );
    assert!(destination_untouched(&dest));
}

#[test]
fn cad1163_acc_restore_refuses_tampered_member_bytes() {
    let root = root("tamper");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let package = export_fixture(&root, &tracker, &state);
    let ticket = package.join("tracker").join(TICKET_REL);
    let mut bytes = fs::read(&ticket).unwrap();
    bytes[0] ^= 0x01;
    fs::write(&ticket, &bytes).unwrap();

    let dest = root.join("dest");
    assert!(
        restore_inactive(&package, &dest, &restore_opts()).is_err(),
        "flipped byte refused"
    );
    assert!(destination_untouched(&dest));
}

#[test]
fn cad1163_acc_restore_refuses_path_traversal_member() {
    let root = root("traversal");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let package = export_fixture(&root, &tracker, &state);
    let mut manifest = read_manifest(&package);
    manifest["members"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"path": "../escape.txt", "bytes": 0, "sha256": sha_hex(b"")}));
    fs::write(
        package.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let dest = root.join("dest");
    assert!(
        restore_inactive(&package, &dest, &restore_opts()).is_err(),
        "traversal member refused"
    );
    assert!(
        !root.join("escape.txt").exists(),
        "nothing written outside destination"
    );
    assert!(destination_untouched(&dest));
}

#[test]
fn cad1163_acc_restore_refuses_nonempty_destination_without_replace() {
    let root = root("nonempty");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let package = export_fixture(&root, &tracker, &state);
    let dest = root.join("dest");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("keep.txt"), b"operator data").unwrap();
    let before = tree(&dest);
    assert!(
        restore_inactive(&package, &dest, &restore_opts()).is_err(),
        "nonempty destination refuses"
    );
    assert_eq!(tree(&dest), before, "existing destination byte-identical");
}

#[test]
fn cad1163_acc_restore_replace_is_refused_without_native_proof() {
    let root = root("replace");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let package = export_fixture(&root, &tracker, &state);
    let dest = root.join("dest");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("keep.txt"), b"operator data").unwrap();
    let before = tree(&dest);
    let opts = PortableRestoreOptions { replace: true };
    assert!(
        restore_inactive(&package, &dest, &opts).is_err(),
        "replace refuses without native proof"
    );
    assert_eq!(tree(&dest), before);
}

#[test]
fn cad1163_acc_restore_missing_tracker_member_refuses_before_any_write() {
    // A listed member gone from the package refuses in verification, before
    // any destination write. This is the pre-write case only: the post-stage
    // window (store staged, tracker or marker failing) needs a fault seam
    // that this slice does not expose, so it is a reported gap, not a pass.
    let root = root("missing-member");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let package = export_fixture(&root, &tracker, &state);
    fs::remove_file(package.join("tracker").join(TICKET_REL)).unwrap();
    let dest = root.join("dest");
    assert!(
        restore_inactive(&package, &dest, &restore_opts()).is_err(),
        "missing listed member refuses"
    );
    assert!(destination_untouched(&dest), "no destination created");
}

#[test]
fn cad1163_acc_git_admission_refuses_include_config() {
    // A config include pointing outside the owned roots is refused by export
    // admission before git reads the tracker.
    let root = root("include-restore");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let outside = root.join("outside.gitconfig");
    fs::write(&outside, "[user]\n\tname = outside\n").unwrap();
    let mut config = fs::read_to_string(tracker.join(".git/config")).unwrap();
    config.push_str(&format!("[include]\n\tpath = {}\n", outside.display()));
    fs::write(tracker.join(".git/config"), config).unwrap();
    let out = root.join("package");
    assert!(
        export_portable(&state, &out, &opts(&tracker)).is_err(),
        "include config refuses export"
    );
    assert!(!out.exists() && no_partial(&out));
}

// ---------- CAD-1163 independent gap checks (appended by the verifier) ----------

/// Synthetic tracker-committed secret-shaped files. The tracker is a git repo
/// that may hold `.env`, private key or token-bearing config. Value is a
/// fixture string, never a real credential. Export must not carry it into the
/// package: either it refuses, or the package omits the file.
#[test]
fn cad1163_acc_export_excludes_or_refuses_tracker_committed_secret_files() {
    let root = root("tracker-secret");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    fs::write(
        tracker.join(".env"),
        "SYNTH-TRACKER-ENV-1163=SYNTH-TRACKER-ENV-VALUE-1163\n",
    )
    .unwrap();
    fs::create_dir_all(tracker.join("pm")).unwrap();
    fs::write(
        tracker.join("pm/pm.yaml"),
        "providers:\n  synthetic:\n    token: SYNTH-PM-TOKEN-1163\n",
    )
    .unwrap();
    git(&tracker, &["add", "-A"]);
    git(&tracker, &["commit", "-q", "-m", "synthetic secrets"]);

    let out = root.join("package");
    match export_portable(&state, &out, &opts(&tracker)) {
        Err(_) => assert!(!out.exists() && no_partial(&out), "refusal writes nothing"),
        Ok(_) => {
            let blob = package_bytes(&out);
            assert!(
                !contains(&blob, "SYNTH-TRACKER-ENV-VALUE-1163"),
                "tracker .env value copied into package"
            );
            assert!(
                !contains(&blob, "SYNTH-PM-TOKEN-1163"),
                "pm token-bearing config copied into package"
            );
            assert!(
                !out.join("tracker/.env").exists(),
                "tracker .env member present"
            );
            assert!(
                !out.join("tracker/pm/pm.yaml").exists(),
                "tracker pm config member present"
            );
        }
    }
}

/// A marker that is present but not valid JSON (or truncated) must refuse
/// daemon start, not read as "no marker".
#[test]
fn cad1163_acc_inactive_marker_malformed_refuses_daemon_start() {
    let root = root("marker-malformed");
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join(INACTIVE_MARKER), b"{\"state\":\"committed_inac").unwrap();
    assert!(
        refuse_if_inactive(&state).is_err(),
        "truncated marker refuses start"
    );
    fs::write(state.join(INACTIVE_MARKER), [0xffu8, 0xfe, 0x00]).unwrap();
    assert!(
        refuse_if_inactive(&state).is_err(),
        "non-UTF-8 marker refuses start"
    );
}

/// Commit a `pm/pm.yaml` with the given body into the synthetic tracker.
fn commit_pm(tracker: &Path, body: &str) {
    fs::create_dir_all(tracker.join("pm")).unwrap();
    fs::write(tracker.join("pm/pm.yaml"), body).unwrap();
    git(tracker, &["add", "-A"]);
    git(tracker, &["commit", "-q", "-m", "synthetic pm config"]);
}

/// A PmConfig-shaped config with only synthetic values and no credentials.
const SAFE_PM_YAML: &str = "schema: 1\n\
notes_dir: /var/www/agent-notes\n\
wiki:\n  max_upload_bytes: 1048576\n\
review:\n  pair:\n    synthetic-worker: synthetic-reviewer\n  never:\n    synthetic-worker: [synthetic-other]\n\
approvals:\n  tailnet_logins: []\n";

/// Positive witness: a safe typed config, with no `.env`, exports and restores
/// its bytes unchanged. The refusals below are judged against this.
#[test]
fn cad1163_acc_safe_pm_config_exports_and_restores_byte_for_byte() {
    let root = root("pm-safe");
    let tracker = make_tracker(&root);
    commit_pm(&tracker, SAFE_PM_YAML);
    let state = make_state(&root, false);

    let out = root.join("package");
    export_portable(&state, &out, &opts(&tracker)).expect("safe pm.yaml exports");
    assert_eq!(
        fs::read(out.join("tracker/pm/pm.yaml")).unwrap(),
        SAFE_PM_YAML.as_bytes(),
        "ordinary pm.yaml bytes preserved"
    );

    let dest = root.join("restored-state");
    restore_inactive(&out, &dest, &restore_opts()).expect("safe package restores");
    assert_eq!(
        fs::read(dest.join("portable-tracker/pm/pm.yaml")).unwrap(),
        SAFE_PM_YAML.as_bytes(),
        "restored pm.yaml bytes preserved"
    );
}

/// A token nested under an allowed top-level key (`review`) is not a key the
/// top-level whitelist can see. Export must refuse at the pm.yaml stage.
#[test]
fn cad1163_acc_export_refuses_nested_token_under_allowed_review_key() {
    let root = root("pm-nested-review");
    let tracker = make_tracker(&root);
    commit_pm(
        &tracker,
        "schema: 1\nreview:\n  pair: {}\n  never: {}\n  token: SYNTH-NESTED-TOKEN-1163\n",
    );
    let state = make_state(&root, false);

    let out = root.join("package");
    let err = export_portable(&state, &out, &opts(&tracker))
        .expect_err("nested token under review refuses export");
    assert!(
        err.to_string().contains("pm/pm.yaml"),
        "refusal names the pm.yaml stage: {err}"
    );
    assert!(!out.exists() && no_partial(&out), "refusal writes nothing");
}

/// A schema of the wrong type is not a valid `PmConfig` and must refuse.
#[test]
fn cad1163_acc_export_refuses_pm_config_wrong_typed_schema() {
    let root = root("pm-wrong-type");
    let tracker = make_tracker(&root);
    commit_pm(&tracker, "schema: SYNTH-SCHEMA-1163\n");
    let state = make_state(&root, false);

    let out = root.join("package");
    let err = export_portable(&state, &out, &opts(&tracker))
        .expect_err("non-integer schema refuses export");
    assert!(
        err.to_string().contains("pm/pm.yaml"),
        "refusal names the pm.yaml stage: {err}"
    );
    assert!(!out.exists() && no_partial(&out), "refusal writes nothing");
}

/// Known refusal: an unknown top-level `providers` key carrying a token.
#[test]
fn cad1163_acc_export_refuses_unknown_top_level_provider_token() {
    let root = root("pm-providers");
    let tracker = make_tracker(&root);
    commit_pm(
        &tracker,
        "schema: 1\nproviders:\n  synthetic:\n    token: SYNTH-PROVIDER-TOKEN-1163\n",
    );
    let state = make_state(&root, false);

    let out = root.join("package");
    let err = export_portable(&state, &out, &opts(&tracker))
        .expect_err("unknown providers key refuses export");
    assert!(
        err.to_string().contains("unknown top-level key providers"),
        "refusal names the top-level key: {err}"
    );
    assert!(!out.exists() && no_partial(&out), "refusal writes nothing");
}

/// The receiver is the policy point. A package whose pm.yaml gained a nested
/// token under `approvals`, with manifest digests recomputed, must still be
/// refused by restore, and the destination stays untouched.
#[test]
fn cad1163_acc_restore_refuses_forged_nested_pm_token_with_recomputed_hashes() {
    let root = root("pm-forged-restore");
    let tracker = make_tracker(&root);
    commit_pm(&tracker, SAFE_PM_YAML);
    let state = make_state(&root, false);
    let package = export_fixture(&root, &tracker, &state);

    // Positive witness: the original config is valid and exported byte-for-byte.
    let original: serde_yaml::Value = serde_yaml::from_str(SAFE_PM_YAML).unwrap();
    assert!(original.get("review").is_some(), "original has review map");
    assert_eq!(
        fs::read(package.join("tracker/pm/pm.yaml")).unwrap(),
        SAFE_PM_YAML.as_bytes(),
        "original config exported before forging"
    );

    // Valid YAML: add a token key under the existing `review` mapping.
    let mut forged = original.clone();
    forged["review"]["token"] = serde_yaml::Value::String("SYNTH-FORGED-TOKEN-1163".into());
    let forged_text = serde_yaml::to_string(&forged).unwrap();
    assert!(forged_text.contains("token"), "forged body carries token");
    fs::write(package.join("tracker/pm/pm.yaml"), forged_text.as_bytes()).unwrap();
    rehash_manifest(&package);

    let dest = root.join("dest");
    let err = restore_inactive(&package, &dest, &restore_opts())
        .expect_err("nested token under review refuses restore with recomputed digests");
    let msg = err.to_string();
    assert!(
        msg.contains("pm/pm.yaml") && !msg.contains("not valid YAML"),
        "refusal is the pm.yaml content stage, not a parse error: {msg}"
    );
    assert!(
        destination_untouched(&dest),
        "refused restore writes nothing"
    );
}

/// Narrowing of the secret-file test: a clean tracker with `.env` absent and a
/// safe pm.yaml exports, then the credential-bearing `.env` is refused with the
/// tracker-path stage, and the refusal writes nothing.
#[test]
fn cad1163_acc_tracker_env_refused_at_path_stage_after_clean_witness() {
    let root = root("env-stage");
    let tracker = make_tracker(&root);
    commit_pm(&tracker, SAFE_PM_YAML);
    let state = make_state(&root, false);
    let clean = root.join("package-clean");
    export_portable(&state, &clean, &opts(&tracker)).expect("clean witness exports");

    fs::write(
        tracker.join(".env"),
        "SYNTH-TRACKER-ENV-1163=SYNTH-ENV-VALUE-1163\n",
    )
    .unwrap();
    git(&tracker, &["add", "-A"]);
    git(&tracker, &["commit", "-q", "-m", "synthetic env"]);

    let out = root.join("package-env");
    let err =
        export_portable(&state, &out, &opts(&tracker)).expect_err("tracker .env refuses export");
    assert!(
        err.to_string().contains("credential-named"),
        "refusal is the credential path stage: {err}"
    );
    assert!(!out.exists() && no_partial(&out), "refusal writes nothing");
}

/// Native PM end to end: a real `Pm::init` tracker with a project, one native
/// ticket and one comment, exported and restored through the portable APIs.
/// Reads go through the native `project::list` and `board::find_issue`, not
/// through a hand-written fixture file.
#[test]
fn cad1163_acc_native_pm_ticket_survives_export_restore_and_reads_back() {
    use crate::issue::{board, project, write, Pm};

    let root = root("native-pm");
    let tracker_dir = root.join("pm");
    let pm = Pm::init(&tracker_dir).expect("native Pm::init");
    write::project_add(&pm, "cad", "CAD", &[], &[], &[], None).expect("native project_add");
    let created = write::new_issue(
        &pm,
        &root,
        Some("cad"),
        "Synthetic native ticket",
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        Some("synthetic body for the native ticket"),
        "cad1163-acceptance",
    )
    .expect("native new_issue");
    let id = created["id"].as_str().expect("issue id").to_string();
    write::add_comment(
        &pm,
        &id,
        "synthetic native comment",
        Some("cad1163-acceptance"),
        None,
        None,
        "cad1163-acceptance",
    )
    .expect("native add_comment");

    let state = make_state(&root, false);
    let src_before = tree(&tracker_dir);

    let out = root.join("package");
    export_portable(&state, &out, &opts(&tracker_dir)).expect("native tracker exports");
    assert_eq!(
        tree(&tracker_dir),
        src_before,
        "export leaves source untouched"
    );

    let dest = root.join("restored-state");
    restore_inactive(&out, &dest, &restore_opts()).expect("native package restores");
    assert_eq!(
        tree(&tracker_dir),
        src_before,
        "restore leaves source untouched"
    );

    let restored = Pm::at(&dest.join("portable-tracker")).expect("restored native PM opens");
    let projects = project::list(&restored.dir).expect("restored project list");
    assert!(
        projects.iter().any(|p| p.key == "cad" && p.prefix == "CAD"),
        "native project survives restore"
    );
    let issue = board::find_issue(&restored.dir, &id).expect("native ticket reads back");
    assert_eq!(
        issue.front.title, "Synthetic native ticket",
        "title preserved"
    );
    assert_eq!(
        issue.front.status, "backlog",
        "native default status preserved"
    );
    assert!(
        issue.body.contains("synthetic body for the native ticket"),
        "body preserved"
    );
    assert!(
        issue
            .comments
            .iter()
            .any(|c| c.body.contains("synthetic native comment")),
        "comment preserved"
    );
}

/// A native PM with its real `issue init` hooks: the exact native bytes export
/// (positive first), and a one-byte edit to the pre-commit hook refuses at the
/// hooks stage, writing nothing and leaving the source tree byte-identical.
#[test]
fn cad1163_acc_native_pm_tampered_hook_refuses_export() {
    use crate::issue::{hooks, write, Pm};

    let root = root("native-hook");
    let tracker_dir = root.join("pm");
    let pm = Pm::init(&tracker_dir).expect("native Pm::init");
    write::project_add(&pm, "cad", "CAD", &[], &[], &[], None).expect("native project_add");
    hooks::install(&tracker_dir).expect("native hooks install");
    let state = make_state(&root, false);

    let good = root.join("package-good");
    export_portable(&state, &good, &opts(&tracker_dir))
        .expect("untampered native hooks export succeeds");

    let hook = tracker_dir.join(".git").join("hooks").join("pre-commit");
    let mut bytes = fs::read(&hook).expect("native pre-commit present");
    bytes.push(b'#');
    fs::write(&hook, &bytes).expect("tamper pre-commit by one byte");
    let src_before = tree(&tracker_dir);

    let out = root.join("package-tampered");
    let err = export_portable(&state, &out, &opts(&tracker_dir))
        .expect_err("tampered native hook refuses export");
    assert!(
        err.to_string().contains("active git hooks"),
        "refusal names the hooks stage: {err}"
    );
    assert!(!out.exists() && no_partial(&out), "refusal writes nothing");
    assert_eq!(
        tree(&tracker_dir),
        src_before,
        "source hook, index and tree unchanged"
    );
}

/// A forged `pm_root` that is not an admitted root refuses before the
/// destination or any target directory is created, with digests recomputed.
#[test]
fn cad1163_acc_restore_refuses_forged_pm_root_with_recomputed_digests() {
    let root = root("forged-pm-root");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let package = export_fixture(&root, &tracker, &state);

    let mut manifest = read_manifest(&package);
    manifest["pm_root"] = Value::String("bogus".into());
    fs::write(
        package.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    rehash_manifest(&package);

    let dest = root.join("dest");
    let err = restore_inactive(&package, &dest, &restore_opts())
        .expect_err("forged pm_root refuses restore");
    assert!(
        err.to_string().contains("not an admitted PM root"),
        "refusal is the pm_root stage: {err}"
    );
    assert!(
        destination_untouched(&dest),
        "no destination or target created"
    );
}

// ---------- native-authorized replacement (CAD-1163 RPC, real dispatch) ----------
//
// Every case drives the real `Shared::dispatch` (caller rule, then
// `operator_connection`, then the handler) under `test_seam::scoped`, which
// is the CAD-482 identity channel and exists only in `test-seam` builds.
// Nothing mints `NativeOperatorProof` here. Auth refusals are asserted
// against the native gate's own message, so an unknown method, a YAML or
// package parse failure, or a missing field cannot pass as auth proof.

/// Native gate refusal text (`operator_connection`), the only auth proof.
#[cfg(feature = "test-seam")]
const AUTH_REFUSED: &str = "is an operator action";

/// A controller daemon over a temp PM dir. Returns the live `Shared`.
#[cfg(feature = "test-seam")]
fn replace_daemon(root: &Path) -> (PathBuf, std::sync::Arc<crate::daemon::Shared>) {
    use crate::daemon::{ServeOptions, Shared};
    let state = root.join("controller");
    fs::create_dir_all(&state).unwrap();
    let pm = root.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(&state, &opts).unwrap();
    (state, shared)
}

/// Replace request params. `digest` values are whatever the caller passes,
/// so the binding cases can plant one wrong value at a time.
#[cfg(feature = "test-seam")]
fn replace_params(
    package: &Path,
    dest: &Path,
    controller: &Path,
    pkg: &str,
    target: &str,
    generation: &str,
) -> Value {
    serde_json::json!({
        "source": package.to_string_lossy(),
        "destination": dest.to_string_lossy(),
        "controller_state": controller.to_string_lossy(),
        "expected_package_digest": pkg,
        "expected_destination_digest": target,
        "expected_generation": generation,
    })
}

/// Every entry under `dir`, file or directory, with its mode and bytes:
/// a complete tree fingerprint including private modes and empty dirs.
#[cfg(feature = "test-seam")]
fn full_tree(dir: &Path) -> BTreeMap<String, (u32, Option<Vec<u8>>)> {
    use std::os::unix::fs::PermissionsExt;
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, (u32, Option<Vec<u8>>)>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            let meta = p.symlink_metadata().unwrap();
            let mode = meta.permissions().mode() & 0o777;
            if meta.is_dir() {
                out.insert(rel, (mode, None));
                walk(base, &p, out);
            } else {
                out.insert(rel, (mode, Some(fs::read(&p).unwrap())));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// A managed portable root to replace: a real export restored inactive,
/// then its marker left in place (a managed root is what replacement names).
#[cfg(feature = "test-seam")]
fn managed_target(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    let tracker = make_tracker(&root.join(format!("{name}-src")));
    let state = make_state(&root.join(format!("{name}-src")), false);
    let package = export_fixture(&root.join(format!("{name}-src")), &tracker, &state);
    restore_inactive(&package, &dir, &restore_opts()).expect("managed target restores inactive");
    // A managed root holds the daemon singleton lock file (never created by
    // replacement); a running daemon would hold it, an idle root has it empty.
    fs::write(dir.join("cadence.lock"), b"").unwrap();
    fs::set_permissions(
        dir.join("cadence.lock"),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    dir
}

/// Digest and generation the operator copies from the read-only preflight.
#[cfg(feature = "test-seam")]
fn preflight_values(
    shared: &std::sync::Arc<crate::daemon::Shared>,
    package: &Path,
    dest: &Path,
    controller: &Path,
) -> (String, String, String) {
    use crate::test_seam::{scoped, Asserted};
    let params = replace_params(package, dest, controller, "", "", "");
    let report = scoped(Asserted::Operator, || {
        shared.dispatch("portable_replace_preflight", &params, std::process::id())
    })
    .expect("operator preflight reads the digests");
    (
        report["package_digest"].as_str().unwrap().to_string(),
        report["destination_digest"].as_str().unwrap().to_string(),
        report["destination_generation"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

/// Agent, unproven and forged-field callers never reach the handler: the
/// native gate refuses first and the target tree is byte-identical.
#[cfg(feature = "test-seam")]
#[test]
fn cad1163_acc_replace_rpc_refuses_agent_unproven_and_forged_callers_before_target() {
    use crate::test_seam::{scoped, Asserted};
    let root = root("replace-auth");
    let (controller, shared) = replace_daemon(&root);
    let package = export_fixture(
        &root,
        &make_tracker(&root.join("t")),
        &make_state(&root.join("t"), false),
    );
    let dest = managed_target(&root, "target");
    let before = full_tree(&dest);
    let (pkg, target, generation) = preflight_values(&shared, &package, &dest, &controller);
    let params = replace_params(&package, &dest, &controller, &pkg, &target, &generation);

    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        let err = scoped(who, || {
            shared.dispatch("portable_replace", &params, std::process::id())
        })
        .expect_err("a non-operator caller must be refused");
        assert!(
            err.to_string().contains(AUTH_REFUSED),
            "auth refusal, got: {err}"
        );
    }
    // Forged identity fields are refused by the same gate, before any read.
    for field in ["as", "operator", "actor", "by"] {
        let mut forged = params.clone();
        forged[field] = Value::String("operator".into());
        let err = scoped(Asserted::Agent("worker".into()), || {
            shared.dispatch("portable_replace", &forged, std::process::id())
        })
        .expect_err("a forged identity field must be refused");
        assert!(
            err.to_string().contains(AUTH_REFUSED) || err.to_string().contains("connection-bound"),
            "forged '{field}' refused by the gate, got: {err}"
        );
    }
    assert_eq!(
        full_tree(&dest),
        before,
        "refused callers leave the target tree unchanged"
    );
}

/// Wrong package, destination or generation binding refuses with the full
/// target tree unchanged (run as the operator, so only the binding fails).
#[cfg(feature = "test-seam")]
#[test]
fn cad1163_acc_replace_rpc_refuses_wrong_binding_with_target_unchanged() {
    use crate::test_seam::{scoped, Asserted};
    let root = root("replace-binding");
    let (controller, shared) = replace_daemon(&root);
    let package = export_fixture(
        &root,
        &make_tracker(&root.join("t")),
        &make_state(&root.join("t"), false),
    );
    let dest = managed_target(&root, "target");
    let before = full_tree(&dest);
    let (pkg, target, generation) = preflight_values(&shared, &package, &dest, &controller);
    let wrong = "f".repeat(64);
    let cases = [
        (
            "package",
            replace_params(&package, &dest, &controller, &wrong, &target, &generation),
        ),
        (
            "destination",
            replace_params(&package, &dest, &controller, &pkg, &wrong, &generation),
        ),
        (
            "generation",
            replace_params(&package, &dest, &controller, &pkg, &target, &wrong),
        ),
    ];
    for (axis, params) in cases {
        let err = scoped(Asserted::Operator, || {
            shared.dispatch("portable_replace", &params, std::process::id())
        })
        .expect_err("a wrong binding must refuse");
        eprintln!("EVIDENCE binding[{axis}] => {err}");
        assert!(
            err.to_string().contains("nothing was written")
                || err.to_string().contains("nothing was replaced"),
            "{axis} binding refused without writing, got: {err}"
        );
        assert_eq!(
            full_tree(&dest),
            before,
            "{axis} refusal leaves the target tree unchanged"
        );
    }
}

/// A running daemon holds `cadence.lock` on the target: the singleton lock
/// refuses replacement and the target tree is unchanged.
#[cfg(feature = "test-seam")]
#[test]
fn cad1163_acc_replace_rpc_refuses_running_target_singleton_lock() {
    use crate::test_seam::{scoped, Asserted};
    use std::os::unix::fs::OpenOptionsExt;
    let root = root("replace-lock");
    let (controller, shared) = replace_daemon(&root);
    let package = export_fixture(
        &root,
        &make_tracker(&root.join("t")),
        &make_state(&root.join("t"), false),
    );
    let dest = managed_target(&root, "target");
    let (pkg, target, generation) = preflight_values(&shared, &package, &dest, &controller);
    let before = full_tree(&dest);
    // Hold the same flock a running daemon holds for its life.
    let holder = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dest.join("cadence.lock"))
        .unwrap();
    let rc = unsafe {
        libc::flock(
            std::os::unix::io::AsRawFd::as_raw_fd(&holder),
            libc::LOCK_EX | libc::LOCK_NB,
        )
    };
    assert_eq!(rc, 0, "the fixture takes the singleton lock");
    let params = replace_params(&package, &dest, &controller, &pkg, &target, &generation);
    let err = scoped(Asserted::Operator, || {
        shared.dispatch("portable_replace", &params, std::process::id())
    })
    .expect_err("a running target must refuse");
    eprintln!("EVIDENCE lock => {err}");
    assert!(
        err.to_string().contains("running"),
        "singleton refusal, got: {err}"
    );
    drop(holder);
    assert_eq!(
        full_tree(&dest),
        before,
        "a locked target is never replaced"
    );
}

/// Positive replacement, operator-asserted through the real handler:
/// incoming data is committed INACTIVE, the complete OLD root (its private
/// modes, empty dirs and bytes) is kept whole as the rollback sibling, and
/// the generation advances. A replay of the now-stale generation refuses
/// and cannot import a second time.
#[cfg(feature = "test-seam")]
#[test]
fn cad1163_acc_replace_rpc_positive_commits_inactive_keeps_full_rollback_and_refuses_replay() {
    use crate::test_seam::{scoped, Asserted};
    let root = root("replace-positive");
    let (controller, shared) = replace_daemon(&root);
    let package = export_fixture(
        &root,
        &make_tracker(&root.join("t")),
        &make_state(&root.join("t"), false),
    );
    let dest = managed_target(&root, "target");
    // Plant a private empty directory inside the managed tracker tree: the
    // rollback must keep it (layout allows only managed top-level names).
    let empty = dest.join("portable-tracker").join("empty-private");
    fs::create_dir_all(&empty).unwrap();
    fs::set_permissions(&empty, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let old_tree = full_tree(&dest);
    let (pkg, target, generation) = preflight_values(&shared, &package, &dest, &controller);
    let params = replace_params(&package, &dest, &controller, &pkg, &target, &generation);

    let result = scoped(Asserted::Operator, || {
        shared.dispatch("portable_replace", &params, std::process::id())
    })
    .expect("operator replacement with exact bindings succeeds");
    assert_eq!(result["replaced"], true);
    assert_eq!(result["state"], "committed_inactive");
    assert_eq!(result["actors_enabled"], false);
    assert_eq!(result["pending_messages_consumed"], 0);
    assert!(
        dest.join(INACTIVE_MARKER).is_file(),
        "incoming root is committed inactive"
    );
    let rollback = PathBuf::from(result["rollback"]["path"].as_str().unwrap());
    assert_eq!(
        full_tree(&rollback),
        old_tree,
        "the complete old root, modes and empty dirs included, is retained as rollback"
    );
    assert!(
        rollback.join("portable-tracker/empty-private").is_dir(),
        "the empty private directory survives in rollback"
    );

    // Replay the same, now stale, generation: it must refuse and import nothing.
    let after = full_tree(&dest);
    let replay = scoped(Asserted::Operator, || {
        shared.dispatch("portable_replace", &params, std::process::id())
    })
    .expect_err("a replayed stale generation must refuse");
    assert!(
        replay.to_string().contains("nothing was written"),
        "stale replay refused without writing, got: {replay}"
    );
    assert_eq!(
        full_tree(&dest),
        after,
        "replay leaves the new root unchanged"
    );
}

/// The package the operator approved is swapped for a different package
/// after its digest was read. `replace_authorized` binds the exact manifest
/// bytes it verifies, so the swapped source refuses before any stage or
/// exchange and the target tree is unchanged.
#[cfg(feature = "test-seam")]
#[test]
fn cad1163_acc_replace_rpc_refuses_source_swapped_after_approved_digest() {
    use crate::test_seam::{scoped, Asserted};
    let root = root("replace-swap");
    let (controller, shared) = replace_daemon(&root);
    let package = export_fixture(
        &root,
        &make_tracker(&root.join("t")),
        &make_state(&root.join("t"), false),
    );
    let dest = managed_target(&root, "target");
    let (pkg, target, generation) = preflight_values(&shared, &package, &dest, &controller);
    let before = full_tree(&dest);
    // Attacker-equivalent swap: a different manifest body under the approved path.
    let mut manifest = read_manifest(&package);
    manifest["tracker_revision"] = Value::String("swapped-after-approval".into());
    fs::write(
        package.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let params = replace_params(&package, &dest, &controller, &pkg, &target, &generation);
    let err = scoped(Asserted::Operator, || {
        shared.dispatch("portable_replace", &params, std::process::id())
    })
    .expect_err("a source swapped after its approved digest must refuse");
    eprintln!("EVIDENCE swap => {err}");
    assert!(
        err.to_string().contains("nothing was written") || err.to_string().contains("digest"),
        "swap refused by the digest or verifier, got: {err}"
    );
    assert_eq!(
        full_tree(&dest),
        before,
        "a swapped source never touches the target"
    );
}

// ---------------------------------------------------------------------------
// CAD-1163 manual activation (independent acceptance, cad1163-devin-haiku-
// acceptance). Written from the ticket and the resolved policy, not from the
// implementation. Positive activation runs through `Shared::dispatch` under the
// CAD-482 operator seam (unit-auth only; ancestry is NOT proven here). Startup
// and receipt checks run the real `serve_with` on owned temp roots.
// ---------------------------------------------------------------------------

/// Pending (non-terminal) message in the source: the reconciliation subject.
const ACT_QUEUED: &str = "msg-queued-1";

/// Exact reconciliation inventory for the single queued message.
fn exact_queued() -> Value {
    serde_json::json!([{"kind": "message", "id": ACT_QUEUED, "status": "queued", "action": "retain_inert"}])
}

/// A source with one queued message beside the completed rows, exported and
/// restored inactive into `root/target` with its daemon lock. Returns
/// (package, destination).
fn activation_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let src = root.join("src");
    let tracker = make_tracker(&src);
    let state = make_state(&src, false);
    let conn = Connection::open(db_file(&state)).unwrap();
    conn.execute(
        "INSERT INTO messages(id,alias,body,source,state,created,turn_id)
         VALUES(?1,'cad-worker','pending CAD-1163 work item','cli','queued',4,NULL)",
        params![ACT_QUEUED],
    )
    .unwrap();
    drop(conn);
    let package = export_fixture(&src, &tracker, &state);
    let dest = root.join("target");
    restore_inactive(&package, &dest, &restore_opts()).expect("fixture restores inactive");
    fs::write(dest.join("cadence.lock"), b"").unwrap();
    fs::set_permissions(
        dest.join("cadence.lock"),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    (package, dest)
}

/// Source and destination state dir paths for the activation fixture.
#[cfg(feature = "test-seam")]
fn activation_source_state(root: &Path) -> PathBuf {
    root.join("src").join("state")
}

/// Message states, agent rows and event count of a state dir, read-only.
fn row_states(state: &Path) -> Value {
    let conn = Connection::open(db_file(state)).unwrap();
    let messages: Vec<(String, String)> = conn
        .prepare("SELECT id, state FROM messages ORDER BY seq")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    let agents: Vec<(String, i64, String, i64)> = conn
        .prepare("SELECT alias, enabled, state, COALESCE(pid, -1) FROM agents ORDER BY alias")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    let events: i64 = conn
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap();
    serde_json::json!({ "messages": messages, "agents": agents, "events": events })
}

/// Activate `dest` directly through the native activation module. Used by the
/// normal-build startup checks, which need an activated root on disk.
fn direct_activate(package: &Path, dest: &Path, controller: &Path) -> Value {
    use crate::backup::portable_activate::{activate, parse_reconciliation, ActivateRequest};
    use crate::backup::portable_replace::{package_digest, snapshot_target};
    let snap = snapshot_target(dest).expect("target snapshot");
    let req = ActivateRequest {
        destination: dest.to_path_buf(),
        controller_state: controller.to_path_buf(),
        expected_package_digest: package_digest(package).unwrap(),
        expected_destination_digest: snap.digest,
        expected_generation: snap.generation,
        execution_mode: "disabled".into(),
        bindings: vec![],
        reconciliation: parse_reconciliation(&exact_queued()).unwrap(),
    };
    activate(package, &req).expect("direct activation of an inactive root")
}

/// The launch-relevant view of `agents`: alias, enabled and pid. Recovery on
/// open rewrites the display `state` column, which is not a launch.
fn launch_view(rows: &Value) -> Value {
    Value::Array(
        rows.as_array()
            .unwrap()
            .iter()
            .map(|r| serde_json::json!([r[0], r[1], r[3]]))
            .collect(),
    )
}

/// Restored, then activated root with its controller dir.
fn activated_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let (package, dest) = activation_fixture(root);
    let controller = root.join("controller");
    fs::create_dir_all(&controller).unwrap();
    direct_activate(&package, &dest, &controller);
    (package, dest)
}

use crate::backup::portable_fence::FENCE_FILE as ACT_FENCE;

/// Daemon options bound to an owned temp PM, so no start touches `~/pm`.
fn owned_opts(root: &Path) -> crate::daemon::ServeOptions {
    let pm = root.join("pm");
    if !pm.join("issues").exists() && !pm.join("pm.yaml").exists() {
        crate::issue::Pm::init(&pm).expect("owned PM fixture");
    }
    let opts = crate::daemon::ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    opts
}

/// Run the real `serve_with` on `state` until its socket is up, then stop it
/// through the daemon's own stop flag. Returns the daemon's own result: an
/// early exit surfaces its real refusal instead of a socket timeout.
fn start_and_stop(state: &Path, opts: crate::daemon::ServeOptions) -> crate::error::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let stop = Arc::new(AtomicBool::new(false));
    let opts = crate::daemon::ServeOptions {
        stop: Some(stop.clone()),
        ..opts
    };
    let dir = state.to_path_buf();
    let handle = std::thread::spawn(move || crate::daemon::serve_with(&dir, opts));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !state.join("cadence.sock").exists() && !handle.is_finished() {
        assert!(
            std::time::Instant::now() < deadline,
            "daemon did not come up"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if handle.is_finished() {
        return handle.join().expect("daemon thread joins");
    }
    stop.store(true, Ordering::SeqCst);
    handle.join().expect("daemon thread joins")
}

/// Real daemon startup on an activated root (normal build): read-only, no
/// message consumed or retried, the same across a restart.
#[test]
fn cad1163_acc_activate_real_daemon_start_is_read_only_and_restart_safe() {
    let root = root("activate-startup");
    let (_package, dest) = activated_fixture(&root);
    let before = row_states(&dest);
    assert_eq!(
        before["messages"][3][1], "queued",
        "fixture holds a pending message"
    );
    start_and_stop(&dest, owned_opts(&root)).expect("activated root starts read-only");
    assert_eq!(
        row_states(&dest)["messages"],
        before["messages"],
        "start consumes nothing"
    );
    start_and_stop(&dest, owned_opts(&root)).expect("activated root restarts read-only");
    let after = row_states(&dest);
    assert_eq!(
        after["messages"], before["messages"],
        "restart replays nothing"
    );
    assert_eq!(
        launch_view(&after["agents"]),
        launch_view(&before["agents"]),
        "no actor is enabled or given a pid"
    );
    eprintln!(
        "EVIDENCE agent state before={} after={}",
        before["agents"][0][2], after["agents"][0][2]
    );
    eprintln!(
        "EVIDENCE startup events before={} after={}",
        before["events"], after["events"]
    );
}

/// A source actor accidentally left enabled=1 in the activated root must still
/// never launch: the persistent fence, not the enabled flag, is what stops it.
#[test]
fn cad1163_acc_activate_enabled_true_actor_never_launches_under_fence() {
    let root = root("activate-enabled");
    let (_package, dest) = activated_fixture(&root);
    let conn = Connection::open(db_file(&dest)).unwrap();
    conn.execute("UPDATE agents SET enabled=1 WHERE alias='cad-worker'", [])
        .unwrap();
    drop(conn);
    let before = row_states(&dest);
    assert_eq!(before["agents"][0][1], 1, "fixture actor is enabled");
    start_and_stop(&dest, owned_opts(&root)).expect("fenced root starts with the actor enabled");
    let after = row_states(&dest);
    assert_eq!(
        after["agents"][0][3], -1,
        "enabled actor gets no pid: never launched"
    );
    assert_eq!(
        launch_view(&after["agents"]),
        launch_view(&before["agents"]),
        "enabled actor is never launched"
    );
    assert_eq!(
        after["messages"], before["messages"],
        "no pending message is replayed"
    );
}

/// Missing, corrupt or generation-mismatched receipt refuses daemon start
/// BEFORE the store is adopted: the database bytes and the socket are untouched.
#[test]
fn cad1163_acc_activate_bad_receipt_refuses_start_before_store_adoption() {
    type Damage = fn(&Path);
    let cases: [(&str, Damage); 3] = [
        ("missing", |dest| {
            fs::remove_file(dest.join(ACT_FENCE)).unwrap()
        }),
        ("corrupt", |dest| {
            fs::write(dest.join(ACT_FENCE), "not json").unwrap()
        }),
        ("generation", |dest| {
            let text = fs::read_to_string(dest.join(ACT_FENCE)).unwrap();
            let mut value: Value = serde_json::from_str(&text).unwrap();
            value["generation"] = Value::String("0".repeat(64));
            fs::write(dest.join(ACT_FENCE), value.to_string()).unwrap();
        }),
    ];
    for (i, (label, damage)) in cases.iter().enumerate() {
        let root = root(&format!("activate-receipt-{i}"));
        let (_package, dest) = activated_fixture(&root);
        damage(&dest);
        let db_before = fs::read(db_file(&dest)).unwrap();
        let err = crate::daemon::serve_with(&dest, owned_opts(&root)).expect_err(label);
        eprintln!("EVIDENCE receipt[{label}] => {err}");
        assert!(
            err.to_string().contains("receipt") || err.to_string().contains("execution"),
            "{label} receipt refused by the fence, got: {err}"
        );
        assert_eq!(
            fs::read(db_file(&dest)).unwrap(),
            db_before,
            "{label} receipt: store is not adopted or written"
        );
        assert!(
            !dest.join("cadence.sock").exists(),
            "{label} receipt: no socket"
        );
    }
}

#[cfg(feature = "test-seam")]
struct ActSeam {
    shared: std::sync::Arc<crate::daemon::Shared>,
    controller: PathBuf,
    package: PathBuf,
    dest: PathBuf,
    pkg: String,
    target: String,
    generation: String,
}

/// A controller daemon plus a fresh activation fixture and the operator-read
/// digests, exactly as an operator copies them from the preflight.
#[cfg(feature = "test-seam")]
fn act_seam(root: &Path) -> ActSeam {
    let (controller, shared) = replace_daemon(root);
    let (package, dest) = activation_fixture(root);
    let (pkg, target, generation) = preflight_values(&shared, &package, &dest, &controller);
    ActSeam {
        shared,
        controller,
        package,
        dest,
        pkg,
        target,
        generation,
    }
}

#[cfg(feature = "test-seam")]
fn act_params(s: &ActSeam, reconciliation: Value, mode: &str, bindings: Value) -> Value {
    serde_json::json!({
        "source": s.package.to_string_lossy(),
        "destination": s.dest.to_string_lossy(),
        "controller_state": s.controller.to_string_lossy(),
        "expected_package_digest": s.pkg,
        "expected_destination_digest": s.target,
        "expected_generation": s.generation,
        "execution_mode": mode,
        "bindings": bindings,
        "reconciliation": reconciliation,
    })
}

#[cfg(feature = "test-seam")]
fn act_as_operator(s: &ActSeam, params: &Value) -> crate::error::Result<Value> {
    use crate::test_seam::{scoped, Asserted};
    scoped(Asserted::Operator, || {
        s.shared
            .dispatch("portable_activate", params, std::process::id())
    })
}

/// Positive manual activation through the operator seam: receipt then the
/// activated marker with a fresh generation; all source and target data kept;
/// a stale replay refuses and writes nothing.
#[cfg(feature = "test-seam")]
#[test]
fn cad1163_acc_activate_rpc_positive_keeps_data_and_refuses_stale_replay() {
    let root = root("activate-positive");
    let s = act_seam(&root);
    let before = full_tree(&s.dest);
    let source_db = fs::read(db_file(&activation_source_state(&root))).unwrap();
    let params = act_params(&s, exact_queued(), "disabled", serde_json::json!([]));

    let out =
        act_as_operator(&s, &params).expect("operator activation with exact bindings succeeds");
    assert_eq!(out["activated"], true);
    assert_eq!(out["state"], "activated_manual_disabled");
    assert_eq!(out["execution_mode"], "disabled");
    assert_eq!(out["actors_enabled"], false);
    assert_eq!(out["pending_effects_replayed"], 0);
    assert_eq!(out["reconciliation_retained"], 1);
    let generation = out["generation"].as_str().unwrap().to_string();
    assert_ne!(
        generation, s.generation,
        "activation mints a fresh generation"
    );

    let receipt: Value =
        serde_json::from_str(&fs::read_to_string(s.dest.join(ACT_FENCE)).unwrap()).unwrap();
    assert_eq!(
        receipt["generation"],
        generation.as_str(),
        "receipt matches the new generation"
    );
    let marker: Value =
        serde_json::from_str(&fs::read_to_string(s.dest.join(INACTIVE_MARKER)).unwrap()).unwrap();
    assert_eq!(marker["state"], "activated_manual_disabled");
    assert_eq!(marker["generation"], generation.as_str());

    // Only the marker and the receipt differ; every tracker, ticket, comment,
    // message and agent byte of the restored root is retained.
    let mut kept_before = before.clone();
    kept_before.remove(INACTIVE_MARKER);
    let mut kept_after = full_tree(&s.dest);
    kept_after.remove(INACTIVE_MARKER);
    kept_after.remove(ACT_FENCE);
    assert_eq!(
        kept_after, kept_before,
        "selected data retained byte for byte"
    );
    assert_eq!(
        fs::read(db_file(&activation_source_state(&root))).unwrap(),
        source_db,
        "source store is untouched"
    );
    assert_eq!(
        row_states(&s.dest)["messages"][3][1],
        "queued",
        "pending message stays queued"
    );

    let after = full_tree(&s.dest);
    let replay = act_as_operator(&s, &params).expect_err("a stale replay must refuse");
    assert!(
        replay.to_string().contains("nothing was written"),
        "stale refusal: {replay}"
    );
    assert_eq!(full_tree(&s.dest), after, "replay writes nothing");
}

/// Non-operator callers, forged identity fields, wrong digests or generation,
/// execution-enabled mode and nonempty bindings each refuse with the target
/// tree unchanged and no receipt written.
#[cfg(feature = "test-seam")]
#[test]
fn cad1163_acc_activate_rpc_refuses_callers_bindings_and_mode_with_target_unchanged() {
    use crate::test_seam::{scoped, Asserted};
    let wrong = "f".repeat(64);
    type Mutate = fn(&mut Value, &str);
    let cases: [(&str, Mutate); 6] = [
        ("package digest", |p, w| {
            p["expected_package_digest"] = Value::String(w.into())
        }),
        ("destination digest", |p, w| {
            p["expected_destination_digest"] = Value::String(w.into())
        }),
        ("generation", |p, w| {
            p["expected_generation"] = Value::String(w.into())
        }),
        ("execution_mode enabled", |p, _| {
            p["execution_mode"] = Value::String("enabled".into())
        }),
        ("nonempty bindings", |p, _| {
            p["bindings"] = serde_json::json!([{"alias": "cad-worker"}])
        }),
        ("forged operator field", |p, _| {
            p["operator"] = Value::String("operator".into())
        }),
    ];
    for (i, (label, mutate)) in cases.iter().enumerate() {
        let root = root(&format!("activate-refuse-{i}"));
        let s = act_seam(&root);
        let before = full_tree(&s.dest);
        let mut params = act_params(&s, exact_queued(), "disabled", serde_json::json!([]));
        mutate(&mut params, &wrong);
        let err = act_as_operator(&s, &params).expect_err(label);
        eprintln!("EVIDENCE activate[{label}] => {err}");
        assert_eq!(full_tree(&s.dest), before, "{label}: target tree unchanged");
        assert!(!s.dest.join(ACT_FENCE).exists(), "{label}: no receipt");
    }
    let root = root("activate-refuse-callers");
    let s = act_seam(&root);
    let before = full_tree(&s.dest);
    let params = act_params(&s, exact_queued(), "disabled", serde_json::json!([]));
    for who in [Asserted::Agent("cad-worker".into()), Asserted::Unproven] {
        let err = scoped(who, || {
            s.shared
                .dispatch("portable_activate", &params, std::process::id())
        })
        .expect_err("a non-operator caller must refuse");
        assert!(
            err.to_string().contains(AUTH_REFUSED),
            "auth refusal: {err}"
        );
    }
    assert_eq!(
        full_tree(&s.dest),
        before,
        "callers leave the target unchanged"
    );
    assert!(!s.dest.join(ACT_FENCE).exists(), "callers write no receipt");
}

/// The reconciliation must be the exact current non-terminal inventory: a
/// fabricated, duplicate, missing, wrong-status, wrong-kind or extra entry
/// refuses before any write. Independent failing cases from the ticket.
#[cfg(feature = "test-seam")]
#[test]
fn cad1163_acc_activate_rpc_refuses_non_exact_reconciliation_before_writes() {
    let cases: [(&str, Value); 6] = [
        (
            "fabricated id",
            serde_json::json!([{"kind": "message", "id": "msg-fabricated-999", "status": "queued", "action": "retain_inert"}]),
        ),
        (
            "duplicate entry",
            serde_json::json!([exact_queued()[0].clone(), exact_queued()[0].clone()]),
        ),
        ("missing inventory", serde_json::json!([])),
        (
            "wrong status",
            serde_json::json!([{"kind": "message", "id": ACT_QUEUED, "status": "completed", "action": "retain_inert"}]),
        ),
        (
            "wrong kind",
            serde_json::json!([{"kind": "job", "id": ACT_QUEUED, "status": "queued", "action": "retain_inert"}]),
        ),
        (
            "extra completed row",
            serde_json::json!([exact_queued()[0].clone(), {"kind": "message", "id": "msg-0001", "status": "completed", "action": "retain_inert"}]),
        ),
    ];
    for (i, (label, reconciliation)) in cases.into_iter().enumerate() {
        let root = root(&format!("activate-recon-{i}"));
        let s = act_seam(&root);
        let before = full_tree(&s.dest);
        let params = act_params(&s, reconciliation, "disabled", serde_json::json!([]));
        let err = act_as_operator(&s, &params).expect_err(label);
        eprintln!("EVIDENCE reconciliation[{label}] => {err}");
        assert_eq!(full_tree(&s.dest), before, "{label}: target tree unchanged");
        assert!(!s.dest.join(ACT_FENCE).exists(), "{label}: no receipt");
    }
}

/// The activated root's fence refuses an execution-capable verb through the
/// real dispatch, so an imported actor cannot be resumed by an operator either.
#[cfg(feature = "test-seam")]
#[test]
fn cad1163_acc_activate_fence_refuses_actor_resume_on_activated_root() {
    let root = root("activate-resume");
    let (_package, dest) = activated_fixture(&root);
    let shared = crate::daemon::Shared::new(&dest, &owned_opts(&root))
        .expect("activated root opens in fenced mode");
    let params = serde_json::json!({"alias": "cad-worker"});
    let err = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        shared.dispatch("agent_resume", &params, std::process::id())
    })
    .expect_err("agent_resume is refused under the execution fence");
    assert!(
        err.to_string().contains("execution-disabled"),
        "fence refusal: {err}"
    );
}

// ---------------------------------------------------------------------------
// CAD-1163 constructor fence: pending effects of every reconciled family stay
// inert across a fenced start. Independent author; written from the ticket
// policy (a manual `disabled` root runs no recovery or replay at boot), not
// from the producer's guard.
// ---------------------------------------------------------------------------

/// Durable pending effects in an activated, fenced root, one per family the
/// shared constructor reconciles: runner receipt, CRM send and delivery,
/// image job with its tool claim, and a `submitting` native nudge. Rows are
/// injected after activation as defense-in-depth; the fence must hold them.
fn seed_pending_effects(dest: &Path, install: &str) -> Value {
    use crate::runner::{self, Intent, Receipt, Requester};
    use crate::slots::SlotKind;
    let run_id = format!("run-{}", "a".repeat(32));
    let intent = Intent {
        runner_id: run_id.clone(),
        project: "cadence".into(),
        recipe: "build".into(),
        kind: SlotKind::Build,
        argv: vec!["true".into()],
        worktree: dest.to_path_buf(),
        cwd: dest.to_path_buf(),
        env: vec![],
        head_sha: "0".repeat(40),
        dirty: false,
        digest: "d".repeat(64),
    };
    let requester = Requester {
        kind: "managed".into(),
        lane: "cad-1163".into(),
    };
    let mut receipt = Receipt::pending(&intent, requester, &dest.join("run.log"), 1.0);
    receipt.state = "running".into();
    runner::write_receipt(dest, &receipt).expect("pending runner receipt");

    let core = Connection::open(db_file(dest)).unwrap();
    core.execute(
        "INSERT INTO crm_sends(install_id,context_id,send_id,state,created,updated)
         VALUES(?1,'ctx-1163','send-1163','sending',1,1)",
        params![install],
    )
    .unwrap();
    core.execute(
        "INSERT INTO app_tool_claims(request_id,install_id,alias,slot,binding_digest,input_digest,call_id,created)
         VALUES('req-1163',?1,'cad-worker','image','bind-1163','in-1163','call-1163',1)",
        params![install],
    )
    .unwrap();
    core.execute(
        "INSERT INTO messages(id,alias,body,source,state,created)
         VALUES('nudge-1163','cad-worker','synthetic nudge','nudge','submitting',1)",
        [],
    )
    .unwrap();
    drop(core);

    drop(crate::store::app_records::RecordStore::open(dest, install).expect("record store"));
    let path = crate::store::app_records::record_db_path(dest, install).unwrap();
    let records = Connection::open(path).unwrap();
    records
        .execute(
            "INSERT INTO app_campaign_sends(context_id,send_id,campaign_id,request_id,content_revision,
               content_digest,audience_freeze_id,audience_digest,connection_id,auth_revision,
               link_revision,link_digest,max_recipients,send_digest,unsubscribe_origin,state,created,updated)
             VALUES('ctx-1163','send-1163','camp-1163','sreq-1163',1,'cd-1163','freeze-1163','ad-1163',
               'conn-1163',1,1,'ld-1163',10,'sd-1163','origin-1163','sending',1,1)",
            [],
        )
        .unwrap();
    records
        .execute(
            "INSERT INTO app_campaign_deliveries(context_id,send_id,customer_id,email,idempotency_key,state,updated)
             VALUES('ctx-1163','send-1163','cust-1163','cust-1163@example.invalid','idem-1163','submitting',1)",
            [],
        )
        .unwrap();
    records
        .execute(
            "INSERT INTO app_social_image_jobs(call_id,install_id,context_id,intent_digest,request_id,
               state,spec_json,created,deadline,updated)
             VALUES('call-1163',?1,'ctx-1163','intent-1163','req-1163','active',
               '{\"authority\":{\"binding\":{\"config\":{\"provider\":\"synthetic-1163\",\"account\":\"acct-1163\"}}}}',
               1,1,1)",
            params![install],
        )
        .unwrap();
    drop(records);
    serde_json::to_value(receipt).unwrap()
}

/// Every pending effect's durable state, plus the reconcile events a
/// recovering boot would write. Read-only.
fn effect_states(dest: &Path, install: &str) -> Value {
    let run = crate::runner::read_receipt(dest, &format!("run-{}", "a".repeat(32))).unwrap();
    let core = Connection::open(db_file(dest)).unwrap();
    let crm: String = core
        .query_row(
            "SELECT state FROM crm_sends WHERE install_id=?1 AND send_id='send-1163'",
            params![install],
            |r| r.get(0),
        )
        .unwrap();
    let nudge: String = core
        .query_row(
            "SELECT state FROM messages WHERE id='nudge-1163'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let events: i64 = core
        .query_row(
            "SELECT count(*) FROM events WHERE kind IN ('runner_unknown','nudge_cancelled')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let records =
        Connection::open(crate::store::app_records::record_db_path(dest, install).unwrap())
            .unwrap();
    let delivery: String = records
        .query_row(
            "SELECT state FROM app_campaign_deliveries WHERE send_id='send-1163'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let campaign: String = records
        .query_row(
            "SELECT state FROM app_campaign_sends WHERE send_id='send-1163'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let image: String = records
        .query_row(
            "SELECT state FROM app_social_image_jobs WHERE call_id='call-1163'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::json!({
        "runner": serde_json::to_value(run).unwrap(),
        "crm_send": crm,
        "campaign_send": campaign,
        "delivery": delivery,
        "image_job": image,
        "nudge": nudge,
        "reconcile_events": events,
    })
}

/// The shared constructor (`serve_with` -> `Shared::new`) starts a fenced root
/// twice. Every pending effect stays exactly as seeded: no runner unknown, no
/// CRM uncertain, no image settle, no nudge close, no reconcile event.
#[test]
fn cad1163_acc_activate_fenced_constructor_leaves_pending_effects_inert() {
    let root = root("activate-fence-effects");
    let (_package, dest) = activated_fixture(&root);
    let install = "inst-1163";
    let runner_before = seed_pending_effects(&dest, install);
    let before = effect_states(&dest, install);
    assert_eq!(
        before["runner"]["state"], "running",
        "seeded runner is in flight"
    );
    assert_eq!(before["crm_send"], "sending", "seeded CRM intent is live");
    assert_eq!(
        before["delivery"], "submitting",
        "seeded delivery is submitting"
    );
    assert_eq!(before["image_job"], "active", "seeded image job is active");
    assert_eq!(before["nudge"], "submitting", "seeded nudge is submitting");
    assert_eq!(
        before["runner"], runner_before,
        "receipt read back as seeded"
    );

    start_and_stop(&dest, owned_opts(&root)).expect("fenced root starts");
    assert_eq!(
        effect_states(&dest, install),
        before,
        "first fenced start leaves every pending effect inert"
    );
    start_and_stop(&dest, owned_opts(&root)).expect("fenced root restarts");
    assert_eq!(
        effect_states(&dest, install),
        before,
        "second fenced start leaves every pending effect inert"
    );
}

// ---------------------------------------------------------------------------
// CAD-1163 combined replace -> activate sequence (independent acceptance, CAD482
// operator UNIT seam via Shared::dispatch; disclosed: not real human proof).
// Written from the ticket: a replaced root's fresh activation must succeed with
// its exact package, digest, snapshot generation and live inventory.
// ---------------------------------------------------------------------------

/// Export -> restore inactive -> replace preflight -> authorized replace (old
/// root kept as validated rollback) -> fresh activation preflight -> activate
/// with the exact package, digests, snapshot generation, empty bindings and the
/// live required inventory. Pre-replacement bindings refuse with the target
/// unchanged; after success a replay of the same bindings refuses unchanged.
/// The replaced marker carries a nonce generation, so the snapshot generation
/// differs from it; `activate` must accept the fresh snapshot anyway.
#[cfg(feature = "test-seam")]
#[test]
fn cad1163_acc_replace_then_activate_rpc_sequence_refuses_stale_and_activates_fresh() {
    use crate::test_seam::{scoped, Asserted};
    let root = root("replace-then-activate");
    let (controller, shared) = replace_daemon(&root);
    let (package, dest) = activation_fixture(&root);
    let old_tree = full_tree(&dest);
    let (pkg, target, generation) = preflight_values(&shared, &package, &dest, &controller);
    let params = replace_params(&package, &dest, &controller, &pkg, &target, &generation);
    let replaced = scoped(Asserted::Operator, || {
        shared.dispatch("portable_replace", &params, std::process::id())
    })
    .expect("operator replacement with exact bindings succeeds");
    assert_eq!(replaced["replaced"], true);
    let rollback = PathBuf::from(replaced["rollback"]["path"].as_str().unwrap());
    assert_eq!(
        full_tree(&rollback),
        old_tree,
        "validated rollback keeps the complete old root"
    );
    let replaced_tree = full_tree(&dest);

    let marker: Value =
        serde_json::from_str(&fs::read_to_string(dest.join(INACTIVE_MARKER)).unwrap()).unwrap();
    let pre = scoped(Asserted::Operator, || {
        shared.dispatch(
            "portable_activate_preflight",
            &serde_json::json!({"destination": dest, "controller_state": controller}),
            std::process::id(),
        )
    })
    .expect("activation preflight reads the replaced root");
    assert_ne!(
        marker["generation"], pre["destination_generation"],
        "replacement marker nonce generation differs from the snapshot generation"
    );
    assert_eq!(
        pre["required_reconciliation"],
        exact_queued(),
        "live inventory is the one queued message"
    );

    let activate_body = |digest: &str, gen: &str| {
        serde_json::json!({
            "source": package.to_string_lossy(),
            "destination": dest.to_string_lossy(),
            "controller_state": controller.to_string_lossy(),
            "expected_package_digest": pkg,
            "expected_destination_digest": digest,
            "expected_generation": gen,
            "execution_mode": "disabled",
            "bindings": [],
            "reconciliation": pre["required_reconciliation"],
        })
    };
    let act = |body: &Value| {
        scoped(Asserted::Operator, || {
            shared.dispatch("portable_activate", body, std::process::id())
        })
    };

    // Pre-replacement bindings name the old root: refused, target unchanged.
    let stale = act(&activate_body(&target, &generation)).expect_err("old bindings must refuse");
    assert!(
        stale.to_string().contains("nothing was written"),
        "stale: {stale}"
    );
    assert_eq!(
        full_tree(&dest),
        replaced_tree,
        "stale bindings leave target unchanged"
    );

    // Fresh bindings from the activation preflight: must succeed.
    let body = activate_body(
        pre["destination_digest"].as_str().unwrap(),
        pre["destination_generation"].as_str().unwrap(),
    );
    let out = act(&body).expect("fresh activation of the replaced root succeeds");
    assert_eq!(out["activated"], true);
    assert_eq!(out["state"], "activated_manual_disabled");
    assert_eq!(out["actors_enabled"], false);
    assert_eq!(out["pending_effects_replayed"], 0);
    let generation = out["generation"].as_str().unwrap().to_string();
    let receipt: Value =
        serde_json::from_str(&fs::read_to_string(dest.join(ACT_FENCE)).unwrap()).unwrap();
    assert_eq!(
        receipt["generation"],
        generation.as_str(),
        "receipt matches activation"
    );
    let activated: Value =
        serde_json::from_str(&fs::read_to_string(dest.join(INACTIVE_MARKER)).unwrap()).unwrap();
    assert_eq!(activated["state"], "activated_manual_disabled");
    assert_eq!(
        activated["generation"],
        generation.as_str(),
        "marker matches receipt"
    );
    let rows = row_states(&dest);
    assert_eq!(
        rows["messages"][3][1], "queued",
        "pending message stays queued"
    );
    for agent in rows["agents"].as_array().unwrap() {
        assert_eq!(agent[1], 0, "no imported actor is enabled");
    }

    // Replaying the now-stale bindings refuses and writes nothing.
    let after = full_tree(&dest);
    let replay = act(&body).expect_err("replay of consumed bindings must refuse");
    assert!(
        replay.to_string().contains("nothing was written"),
        "replay: {replay}"
    );
    assert_eq!(
        full_tree(&dest),
        after,
        "replay leaves the activated root unchanged"
    );
}

#[test]
fn cad1337_acc_export_refuses_unapproved_chat_files_owner_column() {
    let root = root("cad1337-forged-chat-files-owner");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let tracker_status_before = git(
        &tracker,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let control_out = root.join("known-schema-control");
    export_portable(&state, &control_out, &opts(&tracker))
        .expect("known schema-41 fixture exports successfully");
    assert!(
        control_out.is_dir() && no_partial(&control_out),
        "known schema-41 control export succeeds without partial staging"
    );
    {
        let conn = Connection::open(db_file(&state)).unwrap();
        conn.execute(
            "ALTER TABLE chat_files ADD COLUMN cad1337_forged_owner TEXT",
            [],
        )
        .unwrap();
    }
    let out = root.join("package");
    let error = export_portable(&state, &out, &opts(&tracker))
        .expect_err("unapproved private ownership columns must be refused");

    assert_eq!(
        error.to_string(),
        "portable export refuses: table chat_files columns do not match the known catalog",
        "the real known-catalog guard must refuse the forged ownership column"
    );
    assert!(
        !out.exists() && no_partial(&out),
        "refusal leaves no output or partial staging"
    );
    assert_eq!(
        git(
            &tracker,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        tracker_status_before,
        "tracker status unchanged"
    );
}
