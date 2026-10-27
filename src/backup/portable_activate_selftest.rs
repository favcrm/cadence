//! CAD-1163 implementer selftest for manual activation (not the independent
//! acceptance). Uses a real native `Pm::init` fixture exported and restored
//! inactive, then activated execution-disabled. Fixtures live under
//! /tmp/cad1163-activate-selftest; no production state is touched.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::backup::portable::{
    activate_preflight, export_portable, refuse_if_inactive, restore_activate_disabled,
    restore_inactive, PortableExportOptions, PortableRestoreOptions, Scope,
};
use crate::backup::portable_activate::{activate, ActivateRequest};
use crate::backup::portable_fence::{self, Mode, FENCE_FILE};
use crate::backup::portable_replace::{package_digest, snapshot_target};
use crate::issue::{hooks, write, Pm};
use crate::rollout::db_file;
use crate::store::Store;

fn root(name: &str) -> PathBuf {
    let dir = Path::new("/tmp/cad1163-activate-selftest").join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A restored inactive root and its package, built from a native PM.
fn restored(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = root(name);
    let pm_dir = root.join("pm");
    let pm = Pm::init(&pm_dir).expect("native Pm::init");
    hooks::install(&pm.dir).expect("issue init hooks");
    write::project_add(&pm, "cad", "CAD", &[], &[], &[], None).expect("project_add");
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    drop(Store::open(&db_file(&state)).unwrap());
    let package = root.join("package");
    export_portable(
        &state,
        &package,
        &PortableExportOptions {
            scope: Scope::Coordination,
            tracker: pm_dir,
            redact_history_secrets: false,
        },
    )
    .expect("export");
    let dest = root.join("restored");
    restore_inactive(&package, &dest, &PortableRestoreOptions { replace: false }).expect("restore");
    (root, package, dest)
}

fn request(package: &Path, dest: &Path, controller: &Path) -> ActivateRequest {
    let snap = snapshot_target(dest).expect("target snapshot");
    ActivateRequest {
        destination: dest.to_path_buf(),
        controller_state: controller.to_path_buf(),
        expected_package_digest: package_digest(package).unwrap(),
        expected_destination_digest: snap.digest,
        expected_generation: snap.generation,
        execution_mode: "disabled".into(),
        bindings: vec![],
        reconciliation: vec![],
    }
}

#[test]
fn cad1163_activate_positive_writes_receipt_then_activated_marker() {
    let (root, package, dest) = restored("positive");
    let controller = root.join("controller");
    fs::create_dir_all(&controller).unwrap();
    let req = request(&package, &dest, &controller);
    let out = activate(&package, &req).expect("activation");
    assert_eq!(out["execution_mode"], "disabled");
    assert_eq!(out["actors_enabled"], false);
    assert_eq!(out["pending_effects_replayed"], 0);
    // The loader accepts the activated root in fenced mode.
    assert_eq!(portable_fence::load(&dest).unwrap(), Mode::ManualDisabled);
    assert!(dest.join(FENCE_FILE).is_file());
}

#[test]
fn cad1163_activate_replay_of_old_generation_refuses() {
    let (root, package, dest) = restored("replay");
    let controller = root.join("controller");
    fs::create_dir_all(&controller).unwrap();
    let req = request(&package, &dest, &controller);
    activate(&package, &req).expect("first activation");
    let err = activate(&package, &req).expect_err("replay refuses");
    assert!(
        err.to_string().contains("does not match the request"),
        "{err}"
    );
}

#[test]
fn cad1163_activate_refuses_execution_enabled_and_bindings() {
    let (root, package, dest) = restored("enabled");
    let controller = root.join("controller");
    fs::create_dir_all(&controller).unwrap();
    let mut req = request(&package, &dest, &controller);
    req.execution_mode = "enabled".into();
    assert!(activate(&package, &req).is_err());
    let mut req = request(&package, &dest, &controller);
    req.bindings = vec![json!({"alias": "x"})];
    assert!(activate(&package, &req).is_err());
    assert!(!dest.join(FENCE_FILE).exists(), "nothing written");
}

#[test]
fn cad1163_activated_marker_without_receipt_refuses_startup() {
    let (root, package, dest) = restored("nofence");
    let controller = root.join("controller");
    fs::create_dir_all(&controller).unwrap();
    activate(&package, &request(&package, &dest, &controller)).expect("activation");
    fs::remove_file(dest.join(FENCE_FILE)).unwrap();
    assert!(
        portable_fence::load(&dest).is_err(),
        "deleted receipt must refuse"
    );
}

#[test]
fn cad1163_receipt_generation_mismatch_refuses_startup() {
    let (root, package, dest) = restored("mismatch");
    let controller = root.join("controller");
    fs::create_dir_all(&controller).unwrap();
    activate(&package, &request(&package, &dest, &controller)).expect("activation");
    let text = fs::read_to_string(dest.join(FENCE_FILE)).unwrap();
    let forged = text.replace(
        &serde_json::from_str::<serde_json::Value>(&text).unwrap()["generation"]
            .as_str()
            .unwrap()
            .to_string(),
        &"0".repeat(64),
    );
    fs::write(dest.join(FENCE_FILE), forged).unwrap();
    assert!(
        portable_fence::load(&dest).is_err(),
        "mismatched generation must refuse"
    );
}

#[test]
fn cad1163_inactive_root_has_no_receipt_and_still_refuses_daemon() {
    let (_root, _package, dest) = restored("inactive");
    assert!(!dest.join(FENCE_FILE).exists());
    assert!(crate::backup::portable::refuse_if_inactive(&dest).is_err());
}

/// Insert one non-terminal message into the restored (not yet activated)
/// store, the way the restore's own fixture would hold it.
fn seed_queued_message(dest: &Path, id: &str, status: &str) {
    let conn = rusqlite::Connection::open(dest.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "INSERT INTO messages(id,alias,body,source,state,created) VALUES(?1,'cad-worker','synthetic','selftest',?2,0)",
        rusqlite::params![id, status],
    )
    .unwrap();
}

fn plan(kind: &str, id: &str, status: &str) -> serde_json::Value {
    json!({"kind": kind, "id": id, "status": status, "action": "retain_inert"})
}

fn with_plan(req: &mut ActivateRequest, rows: Vec<serde_json::Value>) {
    req.reconciliation =
        crate::backup::portable_activate::parse_reconciliation(&json!(rows)).unwrap();
}

#[test]
fn cad1163_inventory_and_plan_must_match_exactly_before_any_write() {
    use crate::backup::portable_activate::required_inventory;
    let (root, package, dest) = restored("inventory");
    let controller = root.join("controller");
    fs::create_dir_all(&controller).unwrap();
    seed_queued_message(&dest, "msg-live-1", "queued");
    let inv = required_inventory(&dest).expect("inventory");
    assert_eq!(inv.len(), 1, "one live message in the closed inventory");
    assert_eq!(
        (
            inv[0].kind.as_str(),
            inv[0].id.as_str(),
            inv[0].status.as_str()
        ),
        ("message", "msg-live-1", "queued")
    );

    // Missing row: no plan entries for the required message.
    let mut req = request(&package, &dest, &controller);
    assert!(activate(&package, &req).is_err(), "missing row refuses");
    // Fabricated id, wrong status, duplicate, extra: each refuses, no write.
    for rows in [
        vec![plan("message", "msg-fake", "queued")],
        vec![plan("message", "msg-live-1", "running")],
        vec![
            plan("message", "msg-live-1", "queued"),
            plan("message", "msg-live-1", "queued"),
        ],
        vec![
            plan("message", "msg-live-1", "queued"),
            plan("job", "job-extra", "open"),
        ],
    ] {
        with_plan(&mut req, rows);
        assert!(activate(&package, &req).is_err());
        assert!(!dest.join(FENCE_FILE).exists(), "no receipt on refusal");
    }
    // Exact plan activates.
    with_plan(&mut req, vec![plan("message", "msg-live-1", "queued")]);
    activate(&package, &req).expect("exact plan activates");
    assert_eq!(portable_fence::load(&dest).unwrap(), Mode::ManualDisabled);
}

#[test]
fn cad1163_unknown_live_status_refuses_inventory() {
    use crate::backup::portable_activate::required_inventory;
    let (_root, _package, dest) = restored("unknownstatus");
    seed_queued_message(&dest, "msg-weird", "mystery_state");
    assert!(
        required_inventory(&dest).is_err(),
        "unknown status refuses the snapshot"
    );
}

/// A package whose store holds one queued message, so activation must name it.
fn restored_with_queued_message(name: &str) -> (PathBuf, PathBuf) {
    let root = root(name);
    let pm_dir = root.join("pm");
    let pm = Pm::init(&pm_dir).expect("native Pm::init");
    hooks::install(&pm.dir).expect("issue init hooks");
    write::project_add(&pm, "cad", "CAD", &[], &[], &[], None).expect("project_add");
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    drop(Store::open(&db_file(&state)).unwrap());
    let conn = rusqlite::Connection::open(db_file(&state)).unwrap();
    conn.execute(
        "INSERT INTO messages(id,alias,body,source,state,created) VALUES(?1,'cad-worker','synthetic','selftest','queued',0)",
        rusqlite::params!["cad1705-queued"],
    )
    .unwrap();
    drop(conn);
    let package = root.join("package");
    export_portable(
        &state,
        &package,
        &PortableExportOptions {
            scope: Scope::Coordination,
            tracker: pm_dir,
            redact_history_secrets: false,
        },
    )
    .expect("export");
    (root, package)
}

#[test]
fn cad1705_offline_activate_new_destination_is_fenced_and_disabled() {
    let (root, package) = restored_with_queued_message("offline-positive");
    let target = root.join("activated");
    let row = crate::backup::portable_activate::parse_reconciliation(&json!([plan(
        "message",
        "cad1705-queued",
        "queued"
    )]))
    .unwrap();
    let out = restore_activate_disabled(&package, &target, &row).expect("offline activation");
    assert_eq!(out["state"], "activated_manual_disabled");
    assert_eq!(out["restored"], true);
    assert_eq!(out["execution_mode"], "disabled");
    assert_eq!(out["actors_enabled"], false);
    assert_eq!(out["pending_effects_replayed"], 0);
    assert_eq!(out["reconciliation_retained"], 1);
    // Startup admits the root, and the fence makes it manual-disabled.
    assert!(refuse_if_inactive(&target).is_ok());
    assert_eq!(portable_fence::load(&target).unwrap(), Mode::ManualDisabled);
    assert!(target.join(FENCE_FILE).is_file());
}

#[test]
fn cad1705_offline_activate_refuses_every_existing_destination() {
    let (root, package) = restored_with_queued_message("offline-existing");
    let empty = root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let restored_dest = root.join("restored");
    restore_inactive(
        &package,
        &restored_dest,
        &PortableRestoreOptions { replace: false },
    )
    .expect("fixture restores");
    for existing in [&empty, &restored_dest] {
        let err = restore_activate_disabled(&package, existing, &[]).expect_err("refused");
        assert!(err.to_string().contains("destination exists"), "{err}");
    }
    assert!(
        fs::read_dir(&empty).unwrap().next().is_none(),
        "empty untouched"
    );
    assert!(
        !restored_dest.join(FENCE_FILE).exists(),
        "managed root untouched"
    );
    let stages = fs::read_dir(&root)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".portable-stage-")
        })
        .count();
    assert_eq!(stages, 0, "no stage leftovers");
}

#[test]
fn cad1705_offline_activate_refuses_non_exact_reconciliation() {
    let (root, package) = restored_with_queued_message("offline-recon");
    let target = root.join("activated");
    let fabricated = crate::backup::portable_activate::parse_reconciliation(&json!([plan(
        "message",
        "not-in-the-package",
        "queued"
    )]))
    .unwrap();
    let err = restore_activate_disabled(&package, &target, &fabricated)
        .expect_err("fabricated row refused");
    assert!(
        err.to_string().contains("not in the required inventory"),
        "{err}"
    );
    assert!(!target.exists(), "no destination after refusal");

    // The exact row is also refused when its status is wrong.
    let wrong = crate::backup::portable_activate::parse_reconciliation(&json!([plan(
        "message",
        "cad1705-queued",
        "running"
    )]))
    .unwrap();
    assert!(restore_activate_disabled(&package, &target, &wrong).is_err());
    assert!(!target.exists(), "no destination after refusal");
}

#[test]
fn cad1705_offline_activate_refuses_a_target_inside_the_package() {
    let (_root, package) = restored_with_queued_message("offline-overlap");
    let inside = package.join("inside");
    let err = restore_activate_disabled(&package, &inside, &[]).expect_err("overlap refused");
    assert!(
        err.to_string().contains("inside the source package"),
        "{err}"
    );
    assert!(!inside.exists(), "nothing created inside the package");
    // A target that contains the package is refused too.
    let (root, package) = restored_with_queued_message("offline-overlap-parent");
    let err =
        restore_activate_disabled(&package, &root, &[]).expect_err("containing target refused");
    assert!(err.to_string().contains("exists"), "{err}");
}

#[test]
fn cad1705_activate_preflight_lists_the_package_inventory_and_writes_nothing() {
    let (root, package) = restored_with_queued_message("offline-preflight");
    let plan_out = activate_preflight(&package).expect("preflight");
    assert_eq!(plan_out["execution_mode"], "disabled");
    assert_eq!(plan_out["bindings"], json!([]));
    assert_eq!(
        plan_out["required_reconciliation"],
        json!([plan("message", "cad1705-queued", "queued")])
    );
    assert_eq!(
        plan_out["package_digest"],
        json!(package_digest(&package).unwrap())
    );
    // Read-only: no destination and no staging directory appeared.
    assert!(!root.join("activated").exists());
    assert!(fs::read_dir(&root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".portable-stage-")
    }));
}
