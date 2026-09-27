use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

const CONFIG: &str = r#"{"schema":1,"providers":[{"provider":"agenticos","origin":"http://api.internal","manifest_pin":"agenticos-manifest@1/publish_post@2"}]}"#;

#[test]
fn strict_metadata_binds_exact_origin_and_rejects_forged_shapes() {
    let metadata = DeploymentMetadata::parse(CONFIG.as_bytes()).unwrap();
    assert_eq!(metadata.pin("agenticos", "http://api.internal"), Some("agenticos-manifest@1/publish_post@2"));
    assert_eq!(metadata.pin("agenticos", "http://company.internal"), None);
    assert_eq!(metadata.pin("other", "http://api.internal"), None);
    for invalid in [CONFIG.replace("\"schema\":1", "\"schema\":1,\"schema\":1"), CONFIG.replace("\"schema\":1", "\"schema\":1,\"env\":true"), CONFIG.replace("\"http://api.internal\"", "null"), CONFIG.replace("\"schema\":1", "\"schema\":2")] {
        assert!(DeploymentMetadata::parse(invalid.as_bytes()).is_err());
    }
}

#[test]
fn pinned_file_reader_refuses_writable_symlink_and_nonroot_metadata() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = dir.path().join("provider-deployments.json");
    std::fs::write(&file, CONFIG).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let uid = unsafe { libc::geteuid() };
    assert!(read_fixture(dir.path(), "provider-deployments.json", uid).unwrap().is_some());
    if uid != 0 { assert!(read_fixture(dir.path(), "provider-deployments.json", 0).is_err()); }
    assert!(read_fixture(dir.path(), "absent.json", uid).unwrap().is_none());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o620)).unwrap();
    assert!(read_fixture(dir.path(), "provider-deployments.json", uid).is_err());
    symlink(&file, dir.path().join("link.json")).unwrap();
    assert!(read_fixture(dir.path(), "link.json", uid).is_err());
}
