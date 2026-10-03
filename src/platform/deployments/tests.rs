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

// CAD-868: this is a trusted embedding assertion, not a caller-selected mode.
const HOSTED_MEDIA_CONFIG: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@2","transport":"hosted-media-lease@1"}]}"#;

#[test]
fn hosted_media_transport_assertion_accepts_exact_composition() {
    let metadata = DeploymentMetadata::parse(HOSTED_MEDIA_CONFIG.as_bytes())
        .expect("trusted hosted media composition must be admitted");
    assert_eq!(
        metadata.pin("agenticos_external", "http://api.internal"),
        Some("agenticos-external-provider-tools@2")
    );
}

#[test]
fn hosted_media_transport_assertion_refuses_malformed_or_mismatched_composition() {
    let config: serde_json::Value = serde_json::from_str(HOSTED_MEDIA_CONFIG).unwrap();
    for (field, value) in [
        ("transport", serde_json::json!(null)),
        ("transport", serde_json::json!("")),
        ("transport", serde_json::json!("hosted-media-lease@2")),
        ("transport", serde_json::json!(true)),
        (
            "transport",
            serde_json::json!({"mode":"hosted-media-lease@1"}),
        ),
        (
            "transport",
            serde_json::json!({"hosted-media-lease@1":null}),
        ),
        ("transport", serde_json::json!(["hosted-media-lease@1"])),
        ("origin", serde_json::json!(null)),
        ("provider", serde_json::json!("agenticos")),
        ("provider", serde_json::json!("other")),
        (
            "manifest_pin",
            serde_json::json!("agenticos-external-manifest@1"),
        ),
        (
            "manifest_pin",
            serde_json::json!("agenticos-external-provider-tools@3"),
        ),
    ] {
        let mut invalid = config.clone();
        invalid["providers"][0][field] = value;
        assert!(
            DeploymentMetadata::parse(&serde_json::to_vec(&invalid).unwrap()).is_err(),
            "mismatched hosted assertion accepted: {invalid}"
        );
    }
    for origin in [
        "https://api.internal",
        "http://other.internal",
        "http://localhost",
        "http://127.0.0.1",
        "http://api.internal:80",
        "http://api.internal/",
        "http://API.internal",
        "http://api.internal.",
        "http://user@api.internal",
        "http://api.internal?mode=hosted",
        "http://api.internal#hosted",
    ] {
        let mut invalid = config.clone();
        invalid["providers"][0]["origin"] = serde_json::json!(origin);
        assert!(
            DeploymentMetadata::parse(&serde_json::to_vec(&invalid).unwrap()).is_err(),
            "nonliteral hosted origin accepted: {origin}"
        );
    }
    let duplicate = HOSTED_MEDIA_CONFIG.replace(
        "\"transport\":\"hosted-media-lease@1\"",
        "\"transport\":\"hosted-media-lease@1\",\"transport\":\"hosted-media-lease@1\"",
    );
    assert!(DeploymentMetadata::parse(duplicate.as_bytes()).is_err());
}

#[test]
fn hosted_lease_attach_without_external_url_exposes_only_builtin_source_and_media() {
    const ISOLATED: &str = "CADENCE_TEST_CAD868_HOSTED_MEDIA_ISOLATED";
    if std::env::var_os(ISOLATED).is_none() {
        // Follow the existing reaper test's self-relaunch convention: remove
        // the URL only in a child, never race other tests' process environment.
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::deployments::tests::hosted_lease_attach_without_external_url_exposes_only_builtin_source_and_media",
                "--test-threads",
                "1",
                "--nocapture",
            ])
            .env(ISOLATED, "1")
            .env_remove("CADENCE_AGENTICOS_EXTERNAL_URL")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{text}");
        assert!(text.contains("1 passed"), "{text}");
        return;
    }
    assert!(std::env::var_os("CADENCE_AGENTICOS_EXTERNAL_URL").is_none());
    let mut opts = crate::daemon::ServeOptions {
        provider_deployments: Some(
            DeploymentMetadata::parse(HOSTED_MEDIA_CONFIG.as_bytes())
                .expect("trusted hosted media composition must be admitted"),
        ),
        ..Default::default()
    };
    // No daemon, credential enrollment, root-owned file access or network call.
    for config in [
        CONFIG,
        r#"{"schema":1,"providers":[]}"#,
        r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@2"}]}"#,
    ] {
        let metadata = DeploymentMetadata::parse(config.as_bytes()).unwrap();
        assert!(metadata.hosted_media().is_none());
        let mut absent = crate::daemon::ServeOptions {
            provider_deployments: Some(metadata),
            ..Default::default()
        };
        crate::platform::agenticos_external::attach(&mut absent).unwrap();
        assert!(!absent.platforms.contains_key("agenticos_external"));
    }
    let mut conflicted = opts.clone();
    crate::platform::agenticos_external::register_with_deployment_pin(
        &mut conflicted,
        "https://external.example.test",
        None,
    )
    .unwrap();
    assert!(crate::platform::agenticos_external::attach(&mut conflicted).is_err());
    assert!(!conflicted.platforms["agenticos_external"].app_credentialless_account("hosted"));
    // Existing trusted pre-registration still wins when metadata asserts no host mode.
    conflicted.provider_deployments = Some(DeploymentMetadata::parse(CONFIG.as_bytes()).unwrap());
    let registration = conflicted.platforms["agenticos_external"].connection_registration();
    crate::platform::agenticos_external::attach(&mut conflicted).unwrap();
    assert_eq!(
        conflicted.platforms["agenticos_external"].connection_registration(),
        registration
    );
    crate::platform::agenticos_external::attach(&mut opts).unwrap();
    crate::platform::agenticos_external::attach(&mut opts).unwrap();
    assert_eq!(opts.platforms.len(), 1);
    let adapter = opts
        .platforms
        .get("agenticos_external")
        .expect("trusted metadata alone must register hosted media");
    assert_eq!(
        adapter.reported_manifest_version().as_deref(),
        Some("agenticos-external-provider-tools@2")
    );
    let table = adapter.table();
    assert_eq!(table.platform, "agenticos_external");
    // CAD-1060: exactly the reviewed read and draft on one builtin, nothing else.
    assert_eq!(
        table
            .tools
            .iter()
            .map(|t| (t.tool.as_str(), t.effect.as_deref(), t.scopes.join(",")))
            .collect::<Vec<_>>(),
        vec![
            (
                "scrapecreators.instagram.user.posts",
                Some("read"),
                "provider.read".into()
            ),
            ("generate_image", Some("draft"), "provider.draft".into()),
        ]
    );
    let descriptor = adapter.connection_descriptor().unwrap();
    descriptor.validate(table).unwrap();
    assert_eq!(
        serde_json::to_value(&descriptor).unwrap(),
        serde_json::json!({
            "schema":1,"provider":"agenticos_external","revision":"agenticos-hosted-connections/2",
            "enrollment_shapes":[],"builtin_accounts":["hosted"],
            "capabilities":[
                {"id":"social.read","version":1,"tools":["scrapecreators.instagram.user.posts"],"scopes":["provider.read"],"effect":"read","semantics":"metadata_read"},
                {"id":"media.generate","version":1,"tools":["generate_image"],"scopes":["provider.draft"],"effect":"draft","semantics":"preview_only"}
            ],
            "action_mappings":[
                {"capability":"social.read","version":1,"action":"list_posts","resource_kind":"connection_account","tool":"scrapecreators.instagram.user.posts","scopes":["provider.read"],"effect":"read","semantics":"metadata_read","input_contract":"social.posts.query@1","output_contract":"social.posts.receipt@1"},
                {"capability":"media.generate","version":1,"action":"generate_image","resource_kind":"connection_account","tool":"generate_image","scopes":["provider.draft"],"effect":"draft","semantics":"preview_only","input_contract":"media.image.prompt@1","output_contract":"media.image.asset@1"}
            ]
        })
    );
    assert!(adapter
        .execute(
            b"",
            "generate_image",
            &serde_json::json!({}),
            "cad868-test",
            None
        )
        .is_err());
}

#[test]
fn hosted_media_and_publisher_assertions_remain_independent() {
    let mut combined: serde_json::Value = serde_json::from_str(HOSTED_MEDIA_CONFIG).unwrap();
    let publisher: serde_json::Value = serde_json::from_str(CONFIG).unwrap();
    combined["providers"]
        .as_array_mut()
        .unwrap()
        .push(publisher["providers"][0].clone());
    let metadata = DeploymentMetadata::parse(&serde_json::to_vec(&combined).unwrap()).unwrap();
    assert!(metadata.hosted_media().is_some());
    assert_eq!(
        metadata.pin("agenticos", "http://api.internal"),
        Some("agenticos-manifest@1/publish_post@2")
    );
    let metadata = DeploymentMetadata::parse(CONFIG.as_bytes()).unwrap();
    assert!(metadata.hosted_media().is_none());
}

#[test]
fn hosted_media_attach_refuses_every_external_url_composition() {
    const ISOLATED: &str = "CADENCE_TEST_CAD868_URL_CONFLICT";
    if std::env::var_os(ISOLATED).is_none() {
        for url in ["https://external.example.test", "http://api.internal", ""] {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "platform::deployments::tests::hosted_media_attach_refuses_every_external_url_composition", "--test-threads", "1", "--nocapture"])
                .env(ISOLATED, "1")
                .env("CADENCE_AGENTICOS_EXTERNAL_URL", url)
                .output().unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(out.status.success(), "{url:?}: {text}");
            assert!(text.contains("1 passed"), "{text}");
        }
        return;
    }
    let mut opts = crate::daemon::ServeOptions {
        provider_deployments: Some(
            DeploymentMetadata::parse(HOSTED_MEDIA_CONFIG.as_bytes()).unwrap(),
        ),
        ..Default::default()
    };
    assert!(crate::platform::agenticos_external::attach(&mut opts).is_err());
    assert!(!opts.platforms.contains_key("agenticos_external"));
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
