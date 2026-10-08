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
    // Seat the high descriptor even when the inherited soft limit is low.
    // A host whose hard limit cannot support this evidence must fail explicitly,
    // rather than reporting a sweep of a descriptor that was never opened.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    assert!(
        limit.rlim_max > 70000,
        "high-FD custody evidence unavailable: hard RLIMIT_NOFILE cannot seat fd 70000"
    );
    if limit.rlim_cur <= 70000 {
        limit.rlim_cur = 70001;
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
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
    let mut names: Vec<String> = env
        .iter()
        .map(|kv| kv.to_str().unwrap().split('=').next().unwrap().to_owned())
        .collect();
    names.sort();
    let mut expected = vec![
        "CADENCE_ALIAS",
        "CADENCE_BRIEFING_DIR",
        "CADENCE_PM_DIR",
        "CADENCE_SOCKET",
        "GH_CONFIG_DIR",
        "GIT_CONFIG_GLOBAL",
        "GIT_TERMINAL_PROMPT",
        "HOME",
        "PATH",
        "PI_CODING_AGENT_DIR",
        "PI_OFFLINE",
        "PI_SKIP_VERSION_CHECK",
        "TMPDIR",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
    ];
    expected.sort_unstable();
    assert_eq!(names, expected);
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

// ---------------------------------------------------------------------------
// CAD-1188 acceptance: invariants of the retired daemon-side prepared-exec
// stack (src/adapter/pi_guest/{execfd,envp}.rs), asserted against the LIVE
// helper path (OwnedLaunch, derived_env, verify_file, Profile/Routing, the
// authority validators). Each test names the retired test it replaces.
// ---------------------------------------------------------------------------
mod cad1188_ported {
    use super::*;
    use crate::protected_pi_profile::authority::{
        Authorized, GraphFile, ImageProfile, Role, Selection,
    };
    use crate::protected_pi_profile::Routing;
    use std::ffi::OsString;

    const GENERATION: &str = "0123456789abcdef0123456789abcdef";
    const MODEL: &str = "openai-codex/gpt-6.1-sol";

    fn nodev() -> File {
        File::open("/dev/null").unwrap()
    }
    fn launch(node: File, argv: &[&str], env: &[&str]) -> OwnedLaunch {
        OwnedLaunch::new(
            profile(),
            node,
            File::open(".").unwrap().into(),
            argv.iter().map(|s| (*s).to_owned()).collect(),
            env.iter().map(|s| CString::new(*s).unwrap()).collect(),
        )
        .unwrap()
    }
    fn alias_hash(alias: &str) -> String {
        Sha256::digest(alias.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    fn image() -> ImageProfile {
        ImageProfile {
            helper_sha256: [1; 32],
            node_sha256: [2; 32],
            cli: "cli.js".into(),
            extensions: vec!["ext.js".into()],
            files: vec![
                GraphFile {
                    path: "cli.js".into(),
                    size: 10,
                    mode: 0o644,
                    sha256: [3; 32],
                },
                GraphFile {
                    path: "ext.js".into(),
                    size: 10,
                    mode: 0o644,
                    sha256: [4; 32],
                },
                GraphFile {
                    path: "node".into(),
                    size: 100,
                    mode: 0o755,
                    sha256: [2; 32],
                },
            ],
        }
    }
    fn authorized(alias: &str, image: ImageProfile) -> (Authorized, Selection) {
        let selection = Selection {
            alias_sha256: alias_hash(alias),
            generation: GENERATION.into(),
            role: Role::Worker,
            model: MODEL.into(),
        };
        let authorized = Authorized {
            version: 1,
            selection: selection.clone(),
            alias: alias.into(),
            operation: "c".repeat(32),
            supervisor: authority::SUPERVISOR_UID,
            guest: authority::GUEST_UID,
            guest_gid: 21000,
            shared_gid: 21001,
            image,
        };
        (authorized, selection)
    }

    /// Replaces `nul_in_argv_or_env_is_refused`: a NUL is refused wherever the
    /// live path builds argv/env, never exec'd.
    #[test]
    fn cad1188_nul_in_argv_or_env_is_refused_on_the_live_path() {
        // argv: the launch plan refuses an interior NUL.
        let refused = OwnedLaunch::new(
            profile(),
            nodev(),
            File::open(".").unwrap().into(),
            vec!["unit-node".into(), "bad\0arg".into()],
            vec![],
        );
        match refused {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidInput),
            Ok(_) => panic!("argv NUL accepted"),
        }
        // env: the alias is the only caller-derived env value.
        let view = format!("/srv/cadence/guest-views/{}/{GENERATION}", "a".repeat(64));
        assert!(derived_env(&view, "clean-alias").is_ok());
        assert!(derived_env(&view, "bad\0alias").is_err());
        // The owner-authorized alias itself may not carry NUL / newline.
        let (ok, selection) = authorized("clean-alias", image());
        ok.validate(&selection).expect("control authorization");
        for bad in ["bad\0alias", "bad\nalias", "bad\ralias"] {
            let (a, s) = authorized(bad, image());
            assert!(a.validate(&s).is_err(), "{bad:?}");
        }
        // The wire profile cannot carry NUL in a segment or the model route.
        use std::os::unix::ffi::OsStringExt;
        let mut alias = format!("--alias-sha256={}", "a".repeat(63)).into_bytes();
        alias.push(0);
        let rest: Vec<OsString> = vec![
            "--profile".into(),
            "pi-guest".into(),
            OsString::from_vec(alias),
            format!("--generation={}", "b".repeat(32)).into(),
            "--".into(),
            "--mode".into(),
            "rpc".into(),
        ];
        assert!(Profile::parse(&rest).is_err());
        let route: Vec<OsString> = vec![
            "--mode".into(),
            "rpc".into(),
            "--model".into(),
            OsString::from_vec(b"a/b\0c".to_vec()),
        ];
        assert!(Routing::parse(&route).is_err());
    }

    /// Dereference a pointer array against its owned backing.
    fn assert_pointers_resolve(backing: &[CString], ptrs: &[*const libc::c_char]) {
        let mut i = 0usize;
        loop {
            let p = ptrs[i];
            if p.is_null() {
                break;
            }
            let s = unsafe { std::ffi::CStr::from_ptr(p) };
            assert_eq!(s, backing[i].as_c_str());
            i += 1;
        }
        assert_eq!(i, backing.len(), "pointer array shorter than its backing");
        assert!(ptrs[i].is_null(), "pointer array must be NULL-terminated");
        assert_eq!(ptrs.len(), backing.len() + 1);
    }
    fn relocate(l: OwnedLaunch) -> OwnedLaunch {
        let boxed = Box::new(l);
        *boxed
    }

    /// Replaces `argv_and_env_pointers_resolve_through_the_plan`,
    /// `exec_args_carrier_owns_backings_after_plan_is_consumed` and
    /// `pointer_array_entries_are_the_boxed_backing_addresses`.
    #[test]
    fn cad1188_launch_pointers_index_the_owned_backing_and_survive_moves() {
        let plan = launch(
            nodev(),
            &["unit-node", "--mode", "rpc"],
            &["HOME=/x", "CADENCE_ALIAS=w"],
        );
        assert_pointers_resolve(&plan.argv, &plan.argv_p);
        assert_pointers_resolve(&plan.env, &plan.env_p);
        // Entries are the exact addresses of the sealed boxed elements.
        for (i, c) in plan.argv.iter().enumerate() {
            assert_eq!(plan.argv_p[i], c.as_ptr(), "argv_p[{i}]");
        }
        for (i, c) in plan.env.iter().enumerate() {
            assert_eq!(plan.env_p[i], c.as_ptr(), "env_p[{i}]");
        }
        // Moving the whole plan (as exec(self) -> exec_inode(self) does) moves
        // only headers; the heap elements, and so the pointers, stay valid.
        let moved = relocate(relocate(plan));
        assert_pointers_resolve(&moved.argv, &moved.argv_p);
        assert_pointers_resolve(&moved.env, &moved.env_p);
        for (i, c) in moved.argv.iter().enumerate() {
            assert_eq!(moved.argv_p[i], c.as_ptr(), "post-move argv_p[{i}]");
        }
        for (i, c) in moved.env.iter().enumerate() {
            assert_eq!(moved.env_p[i], c.as_ptr(), "post-move env_p[{i}]");
        }
        // Empty tables still yield a lone NULL terminator.
        let empty = launch(nodev(), &[], &[]);
        assert!(empty.argv_p.len() == 1 && empty.argv_p[0].is_null());
        assert!(empty.env_p.len() == 1 && empty.env_p[0].is_null());
    }

    fn pipe_pair() -> (File, File) {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
    }
    /// True once every write end of the pipe is closed. Race-free against fd
    /// number reuse, unlike probing a raw descriptor number.
    fn writers_gone(reader: &File) -> bool {
        for _ in 0..500 {
            let mut p = libc::pollfd {
                fd: reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut p, 1, 0) } > 0 && p.revents & libc::POLLHUP != 0 {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }
    fn writers_present(reader: &File) -> bool {
        let mut p = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut p, 1, 0) };
        n == 0 || p.revents & libc::POLLHUP == 0
    }

    /// Replaces `plan_owns_the_bound_fd_and_drop_closes_it` and
    /// `two_plans_own_distinct_fds`.
    #[test]
    fn cad1188_launch_owns_its_node_fd_and_drop_closes_only_that_fd() {
        let (read_a, write_a) = pipe_pair();
        let (read_b, write_b) = pipe_pair();
        let a = launch(write_a, &["a"], &[]);
        let b = launch(write_b, &["b"], &[]);
        assert_ne!(
            a.node.as_raw_fd(),
            b.node.as_raw_fd(),
            "each plan owns its own live fd"
        );
        assert!(writers_present(&read_a) && writers_present(&read_b));
        drop(a);
        assert!(
            writers_gone(&read_a),
            "dropping the plan must close its node fd"
        );
        assert!(
            writers_present(&read_b),
            "dropping one plan closed another plan's fd"
        );
        drop(b);
        assert!(writers_gone(&read_b));
    }

    fn elf_header(patch: impl FnOnce(&mut [u8; 64])) -> Vec<u8> {
        let mut b = [0u8; 64];
        b[..7].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1]);
        b[16] = 2; // ET_EXEC
        #[cfg(target_arch = "x86_64")]
        {
            b[18] = 62;
        }
        #[cfg(target_arch = "aarch64")]
        {
            b[18] = 183;
        }
        patch(&mut b);
        b.to_vec()
    }
    fn measured(dir: &std::path::Path, name: &str, bytes: &[u8]) -> io::Result<File> {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
        // The digest is always the file's true one, so only the identity gates
        // under test can refuse.
        verify_file(
            File::open(&path).unwrap(),
            unsafe { libc::getuid() },
            0o500,
            digest(&path),
        )
    }

    /// Replaces `elf_claim_is_real_magic_not_just_no_shebang`: the exec object
    /// must be a host-arch 64-bit little-endian ELF executable or PIE; "not a
    /// script" is not proof of an executable.
    #[test]
    fn cad1188_node_identity_is_real_elf_magic_not_just_no_shebang() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        assert!(
            measured(d, "exec", &elf_header(|_| {})).is_ok(),
            "ET_EXEC control"
        );
        assert!(
            measured(d, "pie", &elf_header(|b| b[16] = 3)).is_ok(),
            "ET_DYN control"
        );
        // A real host ELF passes the whole identity gate.
        let real = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        assert!(measured(d, "real", &real).is_ok(), "a real host ELF");
        // A #! script, a non-ELF non-script, and every header field wrong.
        let mut script = b"#!/bin/sh\nexit 0\n".to_vec();
        script.resize(64, b'\n');
        let mut mz = b"MZ fake binary, no shebang".to_vec();
        mz.resize(64, 0);
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("script", script),
            ("not-elf", mz),
            ("bad-magic", elf_header(|b| b[1] = b'X')),
            ("class32", elf_header(|b| b[4] = 1)),
            ("big-endian", elf_header(|b| b[5] = 2)),
            ("ident-version", elf_header(|b| b[6] = 0)),
            ("et-rel", elf_header(|b| b[16] = 1)),
            ("et-core", elf_header(|b| b[16] = 4)),
            ("foreign-machine", elf_header(|b| b[18] = 3)),
            ("too-small", elf_header(|_| {})[..63].to_vec()),
        ];
        for (name, bytes) in cases {
            assert!(measured(d, name, &bytes).is_err(), "{name} was accepted");
        }
    }

    /// Replaces `open_bound_refuses_unset_pin`: an unprovisioned (all-zero)
    /// digest can never describe an executable the helper will bind.
    #[test]
    fn cad1188_unset_image_pin_is_refused() {
        assert!(image().validate().is_ok(), "control image");
        let mut no_helper = image();
        no_helper.helper_sha256 = [0; 32];
        assert!(no_helper.validate().is_err());
        let mut no_node = image();
        no_node.node_sha256 = [0; 32];
        no_node.files[2].sha256 = [0; 32];
        assert!(no_node.validate().is_err());
        // The Node file entry must carry the pinned digest and the exec mode.
        let mut skewed = image();
        skewed.files[2].sha256 = [9; 32];
        assert!(skewed.validate().is_err());
        let mut mode = image();
        mode.files[2].mode = 0o644;
        assert!(mode.validate().is_err());
        // And the measurement itself refuses an all-zero expectation.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node");
        std::fs::write(&path, elf_header(|_| {})).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(verify_file(
            File::open(&path).unwrap(),
            unsafe { libc::getuid() },
            0o500,
            [0; 32]
        )
        .is_err());
    }

    /// Replaces `guest_envp_names_are_the_finite_set`: the guest environment is
    /// an exact enumerated set; no loader, runtime-option, credential or
    /// private-state name can appear, and every path stays under the view.
    #[test]
    fn cad1188_guest_environment_is_the_exact_finite_set() {
        let view = format!(
            "/srv/cadence/guest-views/{}/{GENERATION}",
            alias_hash("routing-alias")
        );
        let env = derived_env(&view, "routing-alias").unwrap();
        let mut pairs = Vec::new();
        for kv in &env {
            let kv = kv.to_str().unwrap();
            let (name, value) = kv.split_once('=').expect("KEY=VAL");
            pairs.push((name.to_owned(), value.to_owned()));
        }
        let mut names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate env name");
        assert_eq!(
            names,
            vec![
                "CADENCE_ALIAS",
                "CADENCE_BRIEFING_DIR",
                "CADENCE_PM_DIR",
                "CADENCE_SOCKET",
                "GH_CONFIG_DIR",
                "GIT_CONFIG_GLOBAL",
                "GIT_TERMINAL_PROMPT",
                "HOME",
                "PATH",
                "PI_CODING_AGENT_DIR",
                "PI_OFFLINE",
                "PI_SKIP_VERSION_CHECK",
                "TMPDIR",
                "XDG_CACHE_HOME",
                "XDG_CONFIG_HOME",
            ]
        );
        for (name, value) in &pairs {
            assert_ne!(name, "CADENCE_STATE_DIR");
            assert!(!name.starts_with("LD_"), "{name}");
            assert!(!name.starts_with("NODE_"), "{name}");
            assert!(
                !name.ends_with("_KEY") && !name.ends_with("_TOKEN"),
                "{name}"
            );
            assert!(!value.contains('\0'));
        }
        let get = |n: &str| pairs.iter().find(|(k, _)| k == n).unwrap().1.clone();
        assert_eq!(get("CADENCE_ALIAS"), "routing-alias");
        for n in [
            "HOME",
            "PI_CODING_AGENT_DIR",
            "XDG_CONFIG_HOME",
            "CADENCE_BRIEFING_DIR",
            "XDG_CACHE_HOME",
            "TMPDIR",
            "GH_CONFIG_DIR",
            "GIT_CONFIG_GLOBAL",
        ] {
            assert!(get(n).starts_with(&format!("{view}/")), "{n}={}", get(n));
        }
        assert_eq!(get("PATH"), "/opt/cadence/bin:/usr/bin:/bin");
        assert_eq!(get("CADENCE_SOCKET"), "/var/lib/cadence/cadence.sock");
        assert_eq!(get("GIT_TERMINAL_PROMPT"), "0");
        assert_eq!(get("PI_OFFLINE"), "1");
    }

    /// Replaces `prepared_exec_argv_is_profile_tokens_then_provider`: the
    /// helper accepts exactly the fixed profile tokens, then `--`, then the
    /// validated routing suffix. No provider flag can become a helper flag and
    /// no program can be prepended.
    #[test]
    fn cad1188_helper_argv_is_profile_tokens_then_routing_only() {
        let alias = alias_hash("w-alias");
        let routing = Routing::for_agent(false, MODEL).unwrap();
        let build = |routing: &[String]| -> Vec<OsString> {
            let mut a: Vec<OsString> = vec![
                "--profile".into(),
                "pi-guest".into(),
                format!("--alias-sha256={alias}").into(),
                format!("--generation={GENERATION}").into(),
                "--".into(),
            ];
            a.extend(routing.iter().map(OsString::from));
            a
        };
        let parsed = Profile::parse(&build(&routing.tokens())).unwrap();
        assert_eq!(parsed.alias_sha256(), alias);
        assert_eq!(parsed.generation(), GENERATION);
        assert_eq!(parsed.routing().tokens(), routing.tokens());
        assert_eq!(parsed.routing().tokens()[..2], ["--mode", "rpc"]);
        assert_eq!(parsed.routing().model(), Some(MODEL));
        let master = Routing::for_agent(true, MODEL).unwrap();
        assert!(Profile::parse(&build(&master.tokens()))
            .unwrap()
            .routing()
            .no_session());

        let rt = |extra: &[&str]| -> Vec<String> {
            let mut t = routing.tokens();
            t.extend(extra.iter().map(|s| (*s).to_owned()));
            t
        };
        for extra in [
            &["--env", "X=1"][..],
            &["--extension", "/tmp/x.js"],
            &["--session", "/tmp/s"],
            &["--print"],
            &["--"],
        ] {
            assert!(Profile::parse(&build(&rt(extra))).is_err(), "{extra:?}");
        }
        let prepended: Vec<String> = ["/usr/bin/node", "--mode", "rpc"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert!(Profile::parse(&build(&prepended)).is_err());
        // Fixed order and the `--` separator are part of the grammar.
        let mut swapped = build(&routing.tokens());
        swapped.swap(2, 3);
        assert!(Profile::parse(&swapped).is_err());
        let mut no_dash = build(&routing.tokens());
        no_dash.remove(4);
        assert!(Profile::parse(&no_dash).is_err());
        // Segments are fixed-charset, fixed-length lowercase hex.
        for bad in [
            format!("--alias-sha256={}", &alias[..63]),
            format!("--alias-sha256={}", alias.to_uppercase()),
            format!("--alias-sha256={}", "../".repeat(21) + "a"),
        ] {
            let mut a = build(&routing.tokens());
            a[2] = bad.clone().into();
            assert!(Profile::parse(&a).is_err(), "{bad}");
        }
    }
}
