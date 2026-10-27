//! Package inspection is not a Native or target-contract admission receipt.
use super::workspace::{bundle_digest, check_offline, inspect_offline};
use crate::issue::app;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

const WORKFLOW: &str = "---\ntitle: Inspect fixture\ngoal: Produce an inert fixture\n---\n\n## Prepare\nagent: fixture-agent\nsize: S\n\nPrepare it.\n\n### Acceptance\n- [ ] fixture prepared\n";

fn write(root: &Path, name: &str, body: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn bundle(root: &Path, requirements: &str, storage: bool) -> BTreeMap<String, String> {
    let mut files = BTreeMap::from([
        (
            "app.md".to_string(),
            format!("---\napp: inspection-fixture\ntitle: Inspection fixture\nversion: '1'\nneeds:\n  connections: []\n{requirements}---\n\nInert fixture.\n"),
        ),
        ("workflows/prepare.md".to_string(), WORKFLOW.to_string()),
    ]);
    if storage {
        // Deliberately opaque: inspection must not claim that Native validates
        // the target-owned app-storage DSL or can execute it.
        files.insert(
            "app-storage.json".to_string(),
            "{\"fixture\":1}".to_string(),
        );
        files.insert(
            "storage/001-initial.json".to_string(),
            "{\"fixture\":1}".to_string(),
        );
    }
    for (name, body) in &files {
        write(root, name, body);
    }
    files
}

fn inspect(root: &Path) -> Value {
    inspect_offline(root.to_str().unwrap()).unwrap()
}

const STORAGE: &str = "  requires: {contracts: {app-storage: [1]}}\n";

#[test]
fn supported_package_inspection_and_native_check_bind_the_same_complete_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let files = bundle(root.path(), "", false);
    let inspected = inspect(root.path());
    let native = check_offline(root.path().to_str().unwrap()).unwrap();
    assert_eq!(inspected["ok"], true, "{inspected}");
    assert_eq!(native["ok"], true, "{native}");
    assert_eq!(inspected["digest"], bundle_digest(&files));
    assert_eq!(native["digest"], inspected["digest"]);
    assert_eq!(inspected["native_compatibility"]["compatible"], true);
    assert_eq!(native["check"], "app-package/1");
}

#[test]
fn storage_inspection_observes_native_refusal_without_target_or_execution_authority() {
    let root = tempfile::tempdir().unwrap();
    let files = bundle(root.path(), STORAGE, true);
    let inspected = inspect(root.path());
    assert_eq!(inspected["ok"], true, "{inspected}");
    assert_eq!(inspected["check"], "app-package-inspection/1");
    assert_eq!(inspected["validation"], "package-integrity");
    assert!(inspected["target_compatibility"].is_null());
    assert!(inspected.get("compatibility").is_none());
    assert_eq!(inspected["native_compatibility"]["compatible"], false);
    assert_eq!(
        inspected["native_compatibility"]["contracts"]["app-storage"],
        serde_json::json!([1])
    );
    assert_eq!(inspected["digest"], bundle_digest(&files));
    let native = check_offline(root.path().to_str().unwrap()).unwrap();
    assert_eq!(native["ok"], false, "{native}");
    assert_eq!(native["refusal"]["code"], "app_check_content");
    assert_eq!(native["compatibility"]["compatible"], false);
    // The filesystem validator used by installed-byte approval remains strict,
    // not just the CLI's default report path.
    assert!(app::validate(root.path(), &HashSet::new(), &[]).is_err());
}

#[test]
fn inspection_digest_binds_both_storage_declaration_and_migration_bytes() {
    let root = tempfile::tempdir().unwrap();
    bundle(root.path(), STORAGE, true);
    let first = inspect(root.path());
    write(root.path(), "app-storage.json", "{\"fixture\":2}");
    let declaration_edit = inspect(root.path());
    assert_eq!(declaration_edit["ok"], true);
    assert_ne!(first["digest"], declaration_edit["digest"]);
    write(root.path(), "storage/001-initial.json", "{\"fixture\":2}");
    let migration_edit = inspect(root.path());
    assert_eq!(migration_edit["ok"], true);
    assert_ne!(declaration_edit["digest"], migration_edit["digest"]);
}

#[test]
fn inspection_does_not_waive_other_unknown_runtime_requirements() {
    let root = tempfile::tempdir().unwrap();
    bundle(
        root.path(),
        "  requires: {contracts: {app-storage: [1], future-runtime: [9]}}\n",
        true,
    );
    let inspected = inspect(root.path());
    assert_eq!(inspected["ok"], true, "{inspected}");
    let checks = inspected["native_compatibility"]["checks"]
        .as_array()
        .unwrap();
    assert!(checks
        .iter()
        .any(|check| check["contract"] == "future-runtime" && check["ok"] == false));
    assert_eq!(
        check_offline(root.path().to_str().unwrap()).unwrap()["ok"],
        false
    );
}

#[test]
fn inspection_refuses_secrets_even_when_the_only_unmet_contract_is_storage() {
    let root = tempfile::tempdir().unwrap();
    bundle(root.path(), STORAGE, true);
    assert_eq!(inspect(root.path())["ok"], true);
    // Synthetic shape-only fixture, never a live credential.
    let noise = format!(
        "{:x}",
        Sha256::digest(b"CAD-1862 synthetic inspection fixture")
    );
    let token = format!("{}{noise}", ["sk", "-proj-"].concat());
    write(
        root.path(),
        "storage/001-initial.json",
        &format!("{{\"token\":\"{token}\"}}"),
    );
    let refused = inspect(root.path());
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["refusal"]["code"], "app_check_content");
}

#[test]
fn inspection_keeps_storage_declaration_pairing_and_exact_major_checks() {
    for requirements in ["", "  requires: {contracts: {app-storage: [2]}}\n"] {
        let root = tempfile::tempdir().unwrap();
        bundle(root.path(), requirements, true);
        let refused = inspect(root.path());
        assert_eq!(refused["ok"], false, "{refused}");
        assert_eq!(refused["refusal"]["code"], "app_check_content");
    }
    let root = tempfile::tempdir().unwrap();
    bundle(root.path(), STORAGE, false);
    let refused = inspect(root.path());
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["refusal"]["code"], "app_check_content");
}

#[test]
fn inspection_still_validates_known_descriptors() {
    let root = tempfile::tempdir().unwrap();
    bundle(root.path(), STORAGE, true);
    write(root.path(), "app-chat.json", "{}");
    let refused = inspect(root.path());
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["refusal"]["code"], "app_check_content");
}

#[test]
fn inspection_refuses_unsafe_inventory_and_oversized_storage() {
    let root = tempfile::tempdir().unwrap();
    bundle(root.path(), STORAGE, true);
    write(root.path(), "storage/nested/change.json", "{}");
    let nested = inspect(root.path());
    assert_eq!(nested["ok"], false, "{nested}");
    assert_eq!(nested["refusal"]["code"], "app_check_inventory");

    let root = tempfile::tempdir().unwrap();
    bundle(root.path(), STORAGE, true);
    write(
        root.path(),
        "storage/large.json",
        &"x".repeat(app::file_cap("storage/large.json") as usize + 1),
    );
    let large = inspect(root.path());
    assert_eq!(large["ok"], false, "{large}");
    assert_eq!(large["refusal"]["code"], "app_check_inventory");
}
