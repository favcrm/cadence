//! Ticket-requested ordinary-UID syscall mechanics, NOT profile authority.
use super::*;
use std::io::{Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;

fn profile() -> Profile {
    Profile::parse(&[
        "--profile".into(),
        "pi-guest".into(),
        format!("--alias-sha256={}", "a".repeat(64)).into(),
        format!("--generation={}", "b".repeat(32)).into(),
        "--".into(),
        "--mode".into(),
        "rpc".into(),
        "--no-session".into(),
    ])
    .unwrap()
}
fn digest(path: &std::path::Path) -> [u8; 32] {
    Sha256::digest(std::fs::read(path).unwrap()).into()
}

/// The subprocess is our UNIT TEST executable, not an installed/setuid helper.
#[test]
#[allow(clippy::disallowed_methods)] // Ordinary unit subprocess to isolate close_fds/exec.
fn ordinary_owned_descriptor_transport_and_failure_cleanup() {
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "ordinary-UID evidence must not run as root"
    );
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("node");
    std::fs::copy(std::env::current_exe().unwrap(), &fixture).unwrap();
    std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o500)).unwrap();
    let hash = digest(&fixture);
    for stage in ["close-open-exec", "failure"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "protected::tests::ordinary_descriptor_probe",
                "--nocapture",
            ])
            .env("CAD1144_UNIT_STAGE", stage)
            .env("CAD1144_UNIT_ROOT", root.path())
            .env(
                "CAD1144_UNIT_DIGEST",
                hash.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            )
            .status()
            .unwrap();
        assert!(
            status.success(),
            "ordinary descriptor stage {stage}: {status}"
        );
    }
}
#[test]
fn ordinary_descriptor_probe() {
    let Ok(stage) = std::env::var("CAD1144_UNIT_STAGE") else {
        return;
    };
    assert_ne!(unsafe { libc::geteuid() }, 0);
    if stage == "after-exec" {
        assert_eq!(
            unsafe { libc::fcntl(70000, libc::F_GETFD) },
            -1,
            "unrelated high descriptor survived"
        );
        assert_eq!(
            unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) },
            1
        );
        return;
    }
    let root = std::path::PathBuf::from(std::env::var("CAD1144_UNIT_ROOT").unwrap());
    if stage == "failure" {
        let path = root.join("non-executable-elf");
        let mut bytes = [0u8; 64];
        bytes[..7].copy_from_slice(&[127, 69, 76, 70, 2, 1, 1]);
        bytes[16] = 2;
        #[cfg(target_arch = "x86_64")]
        {
            bytes[18] = 62;
        }
        #[cfg(target_arch = "aarch64")]
        {
            bytes[18] = 183;
        }
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
        let file = verify_file(
            File::open(&path).unwrap(),
            unsafe { libc::getuid() },
            0o500,
            digest(&path),
        )
        .unwrap();
        let fd = file.as_raw_fd();
        let plan = OwnedLaunch::new(
            profile(),
            file,
            File::open(".").unwrap().into(),
            vec!["unit-node".into()],
            vec![],
        )
        .unwrap();
        assert!(plan.exec_inode().is_err());
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) },
            -1,
            "failed exec leaked owned node"
        );
        let file = File::open(&path).unwrap();
        let fd = file.as_raw_fd();
        assert!(verify_file(file, unsafe { libc::getuid() }, 0o500, [0; 32]).is_err());
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) },
            -1,
            "failed measurement leaked node"
        );
        return;
    }
    let unrelated = File::open(root.join("node")).unwrap();
    assert_eq!(
        unsafe { libc::fcntl(unrelated.as_raw_fd(), libc::F_DUPFD, 70000) },
        70000
    );
    // Do not drop a stale Rust owner after sweeping (its number can be reused).
    std::mem::forget(unrelated);
    super::super::close_fds();
    assert_eq!(unsafe { libc::fcntl(70000, libc::F_GETFD) }, -1);
    let expected = std::env::var("CAD1144_UNIT_DIGEST").unwrap();
    let mut hash = [0; 32];
    for i in 0..32 {
        hash[i] = u8::from_str_radix(&expected[i * 2..i * 2 + 2], 16).unwrap();
    }
    let mut file = File::open(root.join("node")).unwrap();
    file.seek(SeekFrom::Start(123)).unwrap();
    let mut file = verify_file(file, unsafe { libc::getuid() }, 0o500, hash).unwrap();
    assert_eq!(file.stream_position().unwrap(), 123);
    let fd = file.as_raw_fd();
    assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
        0
    ); // Safe ordinary-UID seal only.
    assert!(
        unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
        "owned fd closed at NNP seal"
    );
    std::fs::rename(root.join("node"), root.join("bound-inode")).unwrap();
    std::fs::write(root.join("node"), "attacker replacement, not ELF").unwrap();
    let env = vec![CString::new("CAD1144_UNIT_STAGE=after-exec").unwrap()];
    let plan = OwnedLaunch::new(
        profile(),
        file,
        File::open(&root).unwrap().into(),
        vec![
            "unit-owned-node".into(),
            "--exact".into(),
            "protected::tests::ordinary_descriptor_probe".into(),
            "--nocapture".into(),
        ],
        env,
    )
    .unwrap();
    let error = plan.exec_inode().unwrap_err();
    panic!("owned inode did not replace process: {error}");
}
#[test]
fn private_environment_and_leaf_measurements_are_finite() {
    let view = format!(
        "/srv/cadence/guest-views/{}/{}",
        "a".repeat(64),
        "b".repeat(32)
    );
    let env = derived_env(&view, "routing-alias").unwrap();
    assert_eq!(env.len(), 12);
    for kv in env {
        let name = kv.to_str().unwrap().split('=').next().unwrap();
        assert!(
            !name.starts_with("LD_")
                && !name.starts_with("NODE_")
                && !name.ends_with("_KEY")
                && name != "CADENCE_STATE_DIR"
        );
    }
    let root = tempfile::tempdir().unwrap();
    let bytes = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let path = root.path().join("node");
    std::fs::write(&path, &bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
    let sha: [u8; 32] = Sha256::digest(&bytes).into();
    assert!(verify_file(
        File::open(&path).unwrap(),
        unsafe { libc::getuid() },
        0o755,
        sha
    )
    .is_err());
    std::fs::hard_link(&path, root.path().join("link")).unwrap();
    assert!(verify_file(
        File::open(&path).unwrap(),
        unsafe { libc::getuid() },
        0o500,
        sha
    )
    .is_err());
    std::fs::remove_file(root.path().join("link")).unwrap();
    std::os::unix::fs::symlink(&path, root.path().join("symlink")).unwrap();
    let dir = File::open(root.path()).unwrap();
    assert!(open_child(dir.as_raw_fd(), "symlink", false).is_err());
}
