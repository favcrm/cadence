use super::*;
use std::os::unix::fs::MetadataExt;
use std::sync::mpsc;
use std::time::Duration;

fn grant(token: &str) -> LoginGrant {
    LoginGrant {
        organization_id: "org_accepted".into(),
        slug: "acme".into(),
        endpoint: "https://board.example.test".into(),
        expires_at: 1_900_000_000,
        issuer: "https://issuer.example.test".into(),
        subject_id: "subject_accepted".into(),
        capabilities: vec!["issue.read".into()],
        cli_session: Some(CliSession {
            session_id: "session_accepted".into(),
            refresh_token: "refresh_accepted".into(),
            generation: 1,
            device_label: "acceptance-device".into(),
            idle_expires_at: 1_900_000_000,
            absolute_expires_at: 2_000_000_000,
        }),
        token: token.into(),
    }
}

#[test]
fn login_grant_save_waits_for_the_shared_remote_cli_lock_then_atomically_replaces() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("auth");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();

    grant("old-access").save_credential(&dir).unwrap();
    let credential = dir.join("cli-acme.json");
    let before = std::fs::read(&credential).unwrap();
    let held =
        crate::remote_cli::private_lock(&dir, "acme", Instant::now() + Duration::from_secs(1))
            .unwrap();

    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let writer_dir = dir.clone();
    let writer = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        done_tx
            .send(grant("new-access").save_credential(&writer_dir))
            .unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        done_rx.recv_timeout(Duration::from_millis(150)).is_err(),
        "LoginGrant::save_credential bypassed the remote CLI lock"
    );
    assert_eq!(std::fs::read(&credential).unwrap(), before);

    drop(held);
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("writer did not proceed after lock release")
        .unwrap();
    writer.join().unwrap();

    let after = std::fs::read(&credential).unwrap();
    assert_ne!(after, before);
    assert!(String::from_utf8(after).unwrap().contains("new-access"));
    let metadata = std::fs::symlink_metadata(&credential).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(metadata.mode() & 0o777, 0o600);
}
