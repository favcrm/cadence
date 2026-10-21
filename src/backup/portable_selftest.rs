//! CAD-1163 implementer selftest (not the independent acceptance). A real
//! native `Pm::init` with the installed `issue init` hooks, a project and a
//! ticket with a comment, exported and restored through the portable APIs,
//! then read back through the native `project::list` and `board::find_issue`.
//! Also: an unknown hook body refuses before any git command runs.
//! Fixtures live under /tmp/cad1163-selftest; no production state is touched.

use std::fs;
use std::path::{Path, PathBuf};

use crate::backup::portable::{
    export_portable, restore_inactive, PortableExportOptions, PortableRestoreOptions, Scope,
};
use crate::issue::{board, hooks, project, write, Pm};
use crate::rollout::db_file;
use crate::store::Store;

fn root(name: &str) -> PathBuf {
    let dir = Path::new("/tmp/cad1163-selftest").join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn state_with_store(root: &Path) -> PathBuf {
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    drop(Store::open(&db_file(&state)).unwrap());
    state
}

#[test]
fn cad1163_selftest_native_pm_with_hooks_exports_restores_and_reads_back() {
    let root = root("native");
    let pm_dir = root.join("pm");
    let pm = Pm::init(&pm_dir).expect("native Pm::init");
    hooks::install(&pm.dir).expect("issue init hooks");
    write::project_add(&pm, "cad", "CAD", &[], &[], &[], None).expect("project_add");
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
        Some("synthetic body"),
        "cad1163-selftest",
    )
    .expect("new_issue");
    let id = created["id"].as_str().unwrap().to_string();
    write::add_comment(
        &pm,
        &id,
        "synthetic native comment",
        Some("cad1163-selftest"),
        None,
        None,
        "cad1163-selftest",
    )
    .expect("add_comment");

    let state = state_with_store(&root);
    let out = root.join("package");
    let opts = PortableExportOptions {
        scope: Scope::Coordination,
        tracker: pm_dir.clone(),
        redact_history_secrets: false,
    };
    export_portable(&state, &out, &opts).expect("native PM with hooks exports");
    let dest = root.join("restored");
    let result = restore_inactive(&out, &dest, &PortableRestoreOptions { replace: false })
        .expect("native package restores");
    let pm_root = result["pm_dir"].as_str().expect("pm_dir reported");
    assert_eq!(
        PathBuf::from(pm_root),
        dest.join("portable-tracker"),
        "pm_dir is the validated PM root"
    );

    let restored = Pm::at(Path::new(pm_root)).expect("restored PM opens");
    let projects = project::list(&restored.dir).expect("restored project list");
    assert!(projects.iter().any(|p| p.key == "cad" && p.prefix == "CAD"));
    let issue = board::find_issue(&restored.dir, &id).expect("ticket reads back");
    assert_eq!(issue.front.title, "Synthetic native ticket");
    assert!(issue
        .comments
        .iter()
        .any(|c| c.body.contains("synthetic native comment")));
}

#[test]
fn cad1163_selftest_modified_native_hook_refuses_before_git() {
    let root = root("hook");
    let pm_dir = root.join("pm");
    let pm = Pm::init(&pm_dir).expect("native Pm::init");
    hooks::install(&pm.dir).expect("issue init hooks");
    let hook = pm.dir.join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\necho tampered\n").unwrap();
    let state = state_with_store(&root);
    let opts = PortableExportOptions {
        scope: Scope::Coordination,
        tracker: pm_dir,
        redact_history_secrets: false,
    };
    let err =
        export_portable(&state, &root.join("package"), &opts).expect_err("modified hook refuses");
    assert!(err.to_string().contains("active git hooks"), "{err}");
    assert!(!root.join("package").exists(), "nothing written");
    assert_eq!(
        fs::read_to_string(&hook).unwrap(),
        "#!/bin/sh\necho tampered\n",
        "source unmodified"
    );
}

/// CAD-1441 independent acceptance (author: cad1163-devin-haiku-acceptance,
/// not the implementer). Written from the ticket's known-column contract:
/// the closed catalog compares the EXACT known column names of a table as a
/// set, so physical order (ALTER appends) is not a refusal, while an unknown
/// or a missing known column still refuses. Real entrypoint: `export_portable`
/// runs the real `check_catalog` on a private snapshot. Fixtures are isolated
/// SQLite stores in a unique owned `/tmp/cad1441-*` dir. The historical order and names
/// below are literal from the ticket, not copied from the writer's CATALOG.
const CAD1441_HISTORICAL_DEFS: &[&str] = &[
    "alias TEXT PRIMARY KEY",
    "provider TEXT NOT NULL",
    "endpoint_kind TEXT NOT NULL",
    "role TEXT NOT NULL",
    "cwd TEXT NOT NULL",
    "sandbox TEXT NOT NULL",
    "instructions TEXT",
    "thread_id TEXT",
    "session_id TEXT",
    "model TEXT",
    "pid INTEGER",
    "state TEXT NOT NULL",
    "enabled INTEGER NOT NULL DEFAULT 1",
    "gc_keep INTEGER NOT NULL DEFAULT 0",
    "archived_at REAL",
    "archived_by TEXT",
    "archive_reason TEXT",
    "lifecycle_rev INTEGER NOT NULL DEFAULT 0",
    "error TEXT",
    "created REAL NOT NULL",
    "updated REAL NOT NULL",
    "endpoint TEXT",
    "params TEXT",
    "generation TEXT",
    "effort TEXT",
    "quota TEXT",
    "team_role TEXT",
    "model_selection TEXT",
    "pid_start INTEGER",
    "uid TEXT",
];

const CAD1441_HISTORICAL_NAMES: &[&str] = &[
    "alias",
    "provider",
    "endpoint_kind",
    "role",
    "cwd",
    "sandbox",
    "instructions",
    "thread_id",
    "session_id",
    "model",
    "pid",
    "state",
    "enabled",
    "gc_keep",
    "archived_at",
    "archived_by",
    "archive_reason",
    "lifecycle_rev",
    "error",
    "created",
    "updated",
    "endpoint",
    "params",
    "generation",
    "effort",
    "quota",
    "team_role",
    "model_selection",
    "pid_start",
    "uid",
];

const CAD1441_REFUSAL: &str =
    "portable export refuses: table agents columns do not match the known catalog";

fn cad1441_git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "cad1441-acceptance")
        .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
        .env("GIT_COMMITTER_NAME", "cad1441-acceptance")
        .env("GIT_COMMITTER_EMAIL", "acceptance@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A clean tracker: one commit holding one ticket file.
fn cad1441_tracker(root: &Path) -> PathBuf {
    let tracker = root.join("tracker");
    fs::create_dir_all(tracker.join("tickets")).unwrap();
    cad1441_git(&tracker, &["init", "-q"]);
    fs::write(
        tracker.join("tickets/CAD-1441.md"),
        "# CAD-1441 synthetic ticket fixture\n",
    )
    .unwrap();
    cad1441_git(&tracker, &["add", "-A"]);
    cad1441_git(&tracker, &["commit", "-q", "-m", "CAD-1441 fixture"]);
    tracker
}

/// A real v45 store whose `agents` table is rebuilt from the given literal
/// column definitions. `agents` has no FK, trigger or view in the schema.
fn cad1441_fixture(root: &Path, name: &str, defs: &[&str]) -> PathBuf {
    let state = root.join(name).join("state");
    fs::create_dir_all(&state).unwrap();
    drop(Store::open(&db_file(&state)).unwrap());
    rusqlite::Connection::open(db_file(&state))
        .unwrap()
        .execute_batch(&format!(
            "DROP TABLE agents; CREATE TABLE agents({});",
            defs.join(", ")
        ))
        .unwrap();
    state
}

fn cad1441_names(db: &Path) -> Vec<String> {
    rusqlite::Connection::open(db)
        .unwrap()
        .prepare("SELECT name FROM pragma_table_info('agents') ORDER BY cid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn cad1441_known_catalog_accepts_historical_order_and_refuses_unknown_or_missing_columns() {
    let owned = tempfile::Builder::new()
        .prefix("cad1441-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = owned.path().to_path_buf();
    let tracker = cad1441_tracker(&root);
    let opts = PortableExportOptions {
        scope: Scope::Coordination,
        tracker: tracker.clone(),
        redact_history_secrets: false,
    };

    // Accept: exactly the known names in historical physical order.
    let state = cad1441_fixture(&root, "historical", CAD1441_HISTORICAL_DEFS);
    assert_eq!(
        cad1441_names(&db_file(&state)),
        CAD1441_HISTORICAL_NAMES,
        "fixture carries the historical physical order"
    );
    let good = root.join("package-historical");
    export_portable(&state, &good, &opts)
        .expect("exact known names in historical order must export");
    assert!(good.join("manifest.json").is_file(), "package published");

    // Refuse: one unknown extra column on agents.
    let state = cad1441_fixture(&root, "unknown", CAD1441_HISTORICAL_DEFS);
    rusqlite::Connection::open(db_file(&state))
        .unwrap()
        .execute_batch("ALTER TABLE agents ADD COLUMN reusable_grant TEXT;")
        .unwrap();
    let bad = root.join("package-unknown");
    let err = export_portable(&state, &bad, &opts)
        .expect_err("an unknown agents column must refuse the export");
    assert_eq!(err.to_string(), CAD1441_REFUSAL, "exact catalog refusal");
    assert!(!bad.exists(), "refused export writes no output");

    // Refuse: one known required column (uid) omitted.
    let defs: Vec<&str> = CAD1441_HISTORICAL_DEFS
        .iter()
        .copied()
        .filter(|d| !d.starts_with("uid "))
        .collect();
    let state = cad1441_fixture(&root, "missing", &defs);
    assert_eq!(
        cad1441_names(&db_file(&state)).len(),
        CAD1441_HISTORICAL_NAMES.len() - 1,
        "uid omitted"
    );
    let missing = root.join("package-missing");
    let err = export_portable(&state, &missing, &opts)
        .expect_err("an omitted known agents column must refuse the export");
    assert_eq!(err.to_string(), CAD1441_REFUSAL, "exact catalog refusal");
    assert!(!missing.exists(), "refused export writes no output");
}
