//! CAD-1713: the sandbox-only provider deployment gate.
//!
//! This is the acceptance check for the daemon-enforced rule: a real
//! `load_dev` must refuse a non-dev store, a symlinked metadata input, a
//! non-0600 file and a path outside the sandbox root, and must admit a
//! genuine dev store. The test runs in its own process, so the process
//! environment is isolated from the library test suite.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use cadence_agent::platform::deployments::load_dev;

const METADATA: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"https://api-v2.agenticos.hk","manifest_pin":"agenticos-external-provider-tools@4"}]}"#;

fn write_private(path: &Path) -> PathBuf {
    fs::write(path, METADATA).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    path.to_path_buf()
}

fn set_meta(path: &Path) {
    std::env::set_var("CADENCE_DEV_PROVIDER_DEPLOYMENTS", path);
}

fn write_dev_marker(state: &Path) {
    let marker = serde_json::json!({
        "v": 1,
        "name": "dev",
        "state_dir": fs::canonicalize(state).unwrap(),
    });
    fs::write(state.join(".cadence-dev"), marker.to_string()).unwrap();
}

#[test]
fn dev_provider_gate_refuses_and_admits_through_load_dev() {
    let prior_sandbox = std::env::var_os("CADENCE_SANDBOX_ROOT");
    let prior_meta = std::env::var_os("CADENCE_DEV_PROVIDER_DEPLOYMENTS");

    // A non-dev store is refused before the metadata path is read.
    let plain = tempfile::tempdir().unwrap();
    let plain_state = plain.path().join("state");
    fs::create_dir(&plain_state).unwrap();
    let plain_meta = write_private(&plain.path().join("provider-deployments.json"));
    std::env::set_var("CADENCE_SANDBOX_ROOT", plain.path());
    set_meta(&plain_meta);
    let refused = load_dev(&plain_state).unwrap_err();
    assert!(
        refused.to_string().contains("dev store"),
        "non-dev refusal: {refused}"
    );

    // A real dev store admits the same metadata.
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("dev");
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    fs::write(root.join(".cadence-sandbox"), r#"{"name":"dev"}"#).unwrap();
    write_dev_marker(&state);
    let meta = write_private(&root.join("provider-deployments.json"));
    std::env::set_var("CADENCE_SANDBOX_ROOT", base.path());
    set_meta(&meta);
    let admitted = load_dev(&state).unwrap().expect("dev metadata");
    assert_eq!(
        admitted.pin("agenticos_external", "https://api-v2.agenticos.hk"),
        Some("agenticos-external-provider-tools@4")
    );

    // A symlinked input is refused even when its target is in the sandbox.
    let link = root.join("provider-deployments-link.json");
    symlink(&meta, &link).unwrap();
    set_meta(&link);
    let refused = load_dev(&state).unwrap_err();
    assert!(
        refused.to_string().contains("symlink"),
        "symlink refusal: {refused}"
    );

    // A group/world-readable input is refused.
    let loose = root.join("provider-deployments-loose.json");
    fs::write(&loose, METADATA).unwrap();
    fs::set_permissions(&loose, fs::Permissions::from_mode(0o644)).unwrap();
    set_meta(&loose);
    let refused = load_dev(&state).unwrap_err();
    assert!(
        refused.to_string().contains("0600"),
        "mode refusal: {refused}"
    );

    // A path outside the sandbox root is refused.
    let outside = tempfile::tempdir().unwrap();
    let outside_meta = write_private(&outside.path().join("provider-deployments.json"));
    set_meta(&outside_meta);
    let refused = load_dev(&state).unwrap_err();
    assert!(
        refused.to_string().contains("sandbox root"),
        "containment refusal: {refused}"
    );

    match prior_sandbox {
        Some(value) => std::env::set_var("CADENCE_SANDBOX_ROOT", value),
        None => std::env::remove_var("CADENCE_SANDBOX_ROOT"),
    }
    match prior_meta {
        Some(value) => std::env::set_var("CADENCE_DEV_PROVIDER_DEPLOYMENTS", value),
        None => std::env::remove_var("CADENCE_DEV_PROVIDER_DEPLOYMENTS"),
    }
}
