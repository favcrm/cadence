//! Parent-owned acceptance for the actual installed-screen copy failure.
//! The implementation author must not edit or weaken this check.
use super::copy_verified;

#[test]
fn nested_screen_bundle_copy_preserves_checked_directory_confinement() {
    let scratch = tempfile::tempdir().unwrap();
    let target = scratch.path().join("installed");
    std::fs::create_dir(&target).unwrap();
    let files = vec![
        ("app.md".to_string(), "guide".to_string()),
        ("screens/main/client.js".to_string(), "client".to_string()),
        ("screens/main/styles.css".to_string(), "styles".to_string()),
        ("screens/main/screens.json".to_string(), "{}".to_string()),
    ];
    copy_verified(&files, &target).expect("validated nested screen files must copy");
    for (rel, bytes) in &files {
        assert_eq!(std::fs::read_to_string(target.join(rel)).unwrap(), *bytes);
    }

    let escaped = scratch.path().join("escaped.txt");
    for rel in ["../escaped.txt".to_string(), escaped.display().to_string()] {
        assert!(copy_verified(&[(rel, "forged".to_string())], &target).is_err());
        assert!(!escaped.exists());
    }

    #[cfg(unix)]
    {
        let linked_target = scratch.path().join("linked-install");
        let outside = scratch.path().join("outside");
        std::fs::create_dir(&linked_target).unwrap();
        std::fs::create_dir_all(outside.join("main")).unwrap();
        std::os::unix::fs::symlink(&outside, linked_target.join("screens")).unwrap();
        let screen = vec![("screens/main/client.js".to_string(), "forged".to_string())];
        assert!(copy_verified(&screen, &linked_target).is_err());
        assert!(!outside.join("main/client.js").exists());
    }
}
