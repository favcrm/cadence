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
