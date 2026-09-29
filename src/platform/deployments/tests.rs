use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

const CONFIG: &str = r#"{"schema":1,"providers":[{"provider":"agenticos","origin":"http://api.internal","manifest_pin":"agenticos-manifest@1/publish_post@2"}]}"#;

#[test]
fn nonhosted_registration_never_loads_image_metadata() {
    let mut opts = crate::daemon::ServeOptions::default();
    crate::platform::agenticos::register_with_loader(
        &mut opts,
        "http://localhost:3110",
        false,
        || -> Result<Option<DeploymentMetadata>> {
            panic!("nonhosted composition must not inspect image metadata")
        },
    )
    .unwrap();
    assert_eq!(
        opts.platforms["agenticos"].reported_manifest_version(),
        None
    );
}

#[test]
fn strict_metadata_binds_exact_origin_and_rejects_forged_shapes() {
    let metadata = DeploymentMetadata::parse(CONFIG.as_bytes()).unwrap();
    assert_eq!(
        metadata.pin("agenticos", "http://api.internal"),
        Some("agenticos-manifest@1/publish_post@2")
    );
    assert_eq!(metadata.pin("agenticos", "http://company.internal"), None);
    assert_eq!(metadata.pin("other", "http://api.internal"), None);
    for invalid in [
        CONFIG.replace("\"schema\":1", "\"schema\":1,\"schema\":1"),
        CONFIG.replace("\"schema\":1", "\"schema\":1,\"env\":true"),
        CONFIG.replace("\"http://api.internal\"", "null"),
        CONFIG.replace("\"schema\":1", "\"schema\":2"),
        CONFIG.replace("\"manifest_pin\":", "\"untrusted_field\":"),
        CONFIG.replace(
            "\"provider\":\"agenticos\"",
            "\"provider\":\"agenticos\",\"provider\":\"agenticos\"",
        ),
        String::from(r#"{"schema":1,"providers":null}"#),
        String::from(r#"{"schema":1,"providers":[null]}"#),
    ] {
        assert!(DeploymentMetadata::parse(invalid.as_bytes()).is_err());
    }
}

#[test]
fn external_provider_pin_accepts_only_the_registered_underscore_name() {
    let config = |provider| {
        serde_json::json!({
            "schema": 1,
            "providers": [{
                "provider": provider,
                "origin": "https://api-v2.agenticos.hk",
                "manifest_pin": "agenticos-external-manifest@1"
            }]
        })
    };
    let trusted =
        DeploymentMetadata::parse(&serde_json::to_vec(&config("agenticos_external")).unwrap())
            .expect("the registered external provider must be loadable from trusted metadata");
    assert_eq!(
        trusted.pin("agenticos_external", "https://api-v2.agenticos.hk"),
        Some("agenticos-external-manifest@1")
    );
    assert_eq!(
        trusted.pin("agenticos_external", "https://other.agenticos.hk"),
        None
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = dir.path().join("provider-deployments.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&config("agenticos_external")).unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let loaded = read_fixture(dir.path(), "provider-deployments.json", unsafe {
        libc::geteuid()
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        loaded.pin("agenticos_external", "https://api-v2.agenticos.hk"),
        Some("agenticos-external-manifest@1")
    );
    for provider in ["agenticos_other", "other_provider", "_agenticos_external"] {
        assert!(
            DeploymentMetadata::parse(&serde_json::to_vec(&config(provider)).unwrap()).is_err(),
            "unexpected provider accepted: {provider}"
        );
    }
}

/// CAD-816: media artifacts come from the AgenticOS door itself, so a
/// legacy `image_hosts` key is accepted and ignored rather than breaking
/// parse of an older deployments file.
#[test]
fn legacy_image_hosts_key_is_accepted_and_ignored() {
    let config = serde_json::json!({"schema":1,"providers":[{
        "provider":"agenticos_external","origin":"https://app-v2.agenticos.hk",
        "manifest_pin":"agenticos-external-provider-tools@2",
        "image_hosts":["cdn.example.test"]
    }]});
    let parsed = DeploymentMetadata::parse(&serde_json::to_vec(&config).unwrap()).unwrap();
    assert_eq!(
        parsed.pin("agenticos_external", "https://app-v2.agenticos.hk"),
        Some("agenticos-external-provider-tools@2")
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = dir.path().join("provider-deployments.json");
    std::fs::write(&file, serde_json::to_vec(&config).unwrap()).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let loaded = read_fixture(dir.path(), "provider-deployments.json", unsafe {
        libc::geteuid()
    })
    .unwrap();
    assert!(loaded.is_some());
}

#[test]
fn origin_requires_a_valid_authority_without_caller_url_components() {
    for origin in [
        "http://:",
        "http://[]",
        "http://a:b:c",
        "http://host:99999",
        "http://user@host",
        "http://host/path",
        "http://host?pin=1",
        "http://host#pin",
        "http://host\n",
        "http://-host",
    ] {
        let invalid = serde_json::json!({"schema":1,"providers":[{"provider":"agenticos","origin":origin,"manifest_pin":"pin"}]});
        assert!(
            DeploymentMetadata::parse(&serde_json::to_vec(&invalid).unwrap()).is_err(),
            "{origin:?}"
        );
    }
    for origin in [
        "http://api.internal",
        "http://127.0.0.1:3110",
        "https://[::1]:443",
    ] {
        let valid = serde_json::json!({"schema":1,"providers":[{"provider":"agenticos","origin":origin,"manifest_pin":"pin"}]});
        assert!(DeploymentMetadata::parse(&serde_json::to_vec(&valid).unwrap()).is_ok());
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
    assert!(read_fixture(dir.path(), "provider-deployments.json", uid)
        .unwrap()
        .is_some());
    if uid != 0 {
        assert!(read_fixture(dir.path(), "provider-deployments.json", 0).is_err());
    }
    assert!(read_fixture(dir.path(), "absent.json", uid)
        .unwrap()
        .is_none());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o620)).unwrap();
    assert!(read_fixture(dir.path(), "provider-deployments.json", uid).is_err());
    symlink(&file, dir.path().join("link.json")).unwrap();
    assert!(read_fixture(dir.path(), "link.json", uid).is_err());
    symlink(dir.path(), dir.path().join("linked-directory")).unwrap();
    let root = std::fs::File::open(dir.path()).unwrap();
    assert!(read_components(
        root,
        &["linked-directory", "provider-deployments.json"],
        uid
    )
    .is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let root = std::fs::File::open(dir.path()).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o720)).unwrap();
    assert!(read_components(root, &["provider-deployments.json"], uid).is_err());
}

#[test]
fn pinned_directory_is_not_replaced_by_later_path_substitution() {
    let parent = tempfile::tempdir().unwrap();
    let original = parent.path().join("original");
    std::fs::create_dir(&original).unwrap();
    std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(original.join("metadata.json"), CONFIG).unwrap();
    std::fs::set_permissions(
        original.join("metadata.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let pinned = std::fs::File::open(&original).unwrap();
    std::fs::rename(&original, parent.path().join("retained")).unwrap();
    std::fs::create_dir(&original).unwrap();
    std::fs::write(original.join("metadata.json"), "untrusted replacement").unwrap();
    let metadata = read_components(pinned, &["metadata.json"], unsafe { libc::geteuid() })
        .unwrap()
        .unwrap();
    assert_eq!(
        metadata.pin("agenticos", "http://api.internal"),
        Some("agenticos-manifest@1/publish_post@2")
    );
}
