//! Independently authored lifecycle-swe artifact4 checks. Mounted subset
//! uses the real write_private entry; Reserved-API drafts await owner revision.
use super::*;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

fn private_state() -> (TempDir, std::path::PathBuf) {
    let root = TempDir::new().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    (root, state)
}
fn bytes_of(p: &std::path::Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|_| Vec::new())
}
#[test]
fn write_private_does_not_clobber_existing_final() {
    let (_r, state) = private_state();
    let dir = state.join("out");
    std::fs::create_dir_all(&dir).unwrap();
    let final_path = dir.join("manifest.json");
    std::fs::write(&final_path, b"PRE-EXISTING").unwrap();
    assert!(write_private(&final_path, b"new-manifest").is_err());
    assert_eq!(bytes_of(&final_path), b"PRE-EXISTING");
}
#[test]
fn write_private_refuses_planted_temp() {
    let (_r, state) = private_state();
    let dir = state.join("out");
    std::fs::create_dir_all(&dir).unwrap();
    let final_path = dir.join("manifest.json");
    let tmp = dir.join("manifest.json.partial");
    std::fs::write(&tmp, b"PLANTED-TEMP").unwrap();
    assert!(write_private(&final_path, b"new-manifest").is_err());
    assert_eq!(bytes_of(&tmp), b"PLANTED-TEMP");
    assert!(!final_path.exists());
}
#[test]
fn write_private_happy_path() {
    let (_r, state) = private_state();
    let dir = state.join("out");
    std::fs::create_dir_all(&dir).unwrap();
    let final_path = dir.join("manifest.json");
    write_private(&final_path, b"{\"ok\":true}").unwrap();
    assert_eq!(bytes_of(&final_path), b"{\"ok\":true}");
    assert!(!dir.join("manifest.json.partial").exists());
}
