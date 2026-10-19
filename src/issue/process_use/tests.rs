use super::*;

/// CAD-1196: PID 2's empty ns link is replaced by the caller's NSpid
/// having exactly one entry; nested, absent, duplicated or zero fail.
#[cfg(target_os = "linux")]
#[test]
fn nspid_proof_needs_exactly_one_entry() {
    assert!(status_has_single_nspid(
        "Name:\tx\nNSpid:\t1234\nSeccomp:\t0\n"
    ));
    assert!(!status_has_single_nspid("NSpid:\t1234\t7\n"));
    assert!(!status_has_single_nspid("NSpid:\t0\n"));
    assert!(!status_has_single_nspid("NSpid:\n"));
    assert!(!status_has_single_nspid("NSpid:\tx\n"));
    assert!(!status_has_single_nspid("Name:\tx\n"));
    assert!(!status_has_single_nspid("NSpid:\t1\nNSpid:\t1\n"));
}

/// CAD-1196: PermissionDenied is explained only by a cwd entry the
/// kernel gave to another uid/gid (foreign or non-dumpable process) or
/// by extra permitted capabilities; status credentials never excuse.
#[cfg(target_os = "linux")]
#[test]
fn policy_explains_only_what_the_kernel_rule_denies() {
    let caller = Caller {
        euid: 1000,
        egid: 1000,
        cap_prm: Some(0),
    };
    let caps = |cap: &str| format!("CapPrm:\t{cap}\n");
    let none = caps("0000000000000000");
    assert!(!policy_denies_read(&none, caller, Some((1000, 1000))));
    assert!(!policy_denies_read(&none, caller, None));
    // Entry owned by root (non-dumpable) or by another uid or gid.
    assert!(policy_denies_read(&none, caller, Some((0, 0))));
    assert!(policy_denies_read(&none, caller, Some((1001, 1000))));
    assert!(policy_denies_read(&none, caller, Some((1000, 4))));
    // A status claiming a foreign uid does not excuse an own-owned entry.
    let status = "Uid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nCapPrm:\t0\n";
    assert!(!policy_denies_read(status, caller, Some((1000, 1000))));
    // Extra permitted capability; a subset is not an explanation.
    let extra = caps("0000000800000000");
    assert!(policy_denies_read(&extra, caller, Some((1000, 1000))));
    let held = Caller {
        cap_prm: Some(0x8_0000_0000),
        ..caller
    };
    assert!(!policy_denies_read(&extra, held, Some((1000, 1000))));
    // Unknown caller caps or a malformed target line never excuse.
    let blind = Caller {
        cap_prm: None,
        ..caller
    };
    assert!(!policy_denies_read(&extra, blind, Some((1000, 1000))));
    assert!(!policy_denies_read(
        "CapPrm:\tzz\n",
        caller,
        Some((1000, 1000))
    ));
}

/// CAD-1196 reviewer-written acceptance check for the process-enumeration
/// gate: every refusal is driven through `process_use_under_from_proc_root`
/// (the real guard) over a synthetic proc tree that starts from a state
/// where the scan completes cleanly, so each case isolates one guard.
#[cfg(all(test, target_os = "linux"))]
mod cad1196_acceptance {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use tempfile::{Builder, TempDir};

    struct Proc {
        _root: TempDir,
        proc_root: PathBuf,
        lane: PathBuf,
    }

    impl Proc {
        fn new() -> Self {
            let root = Builder::new().prefix("c1196u-").tempdir_in("/tmp").unwrap();
            let proc_root = root.path().join("proc");
            let lane = root.path().join("lane");
            std::fs::create_dir_all(&lane).unwrap();
            std::fs::create_dir_all(proc_root.join("self/ns")).unwrap();
            std::fs::create_dir_all(proc_root.join("2/ns")).unwrap();
            let canon = proc_root.canonicalize().unwrap();
            let p = Self {
                _root: root,
                proc_root,
                lane,
            };
            p.mountinfo(&canon, "rw");
            std::fs::write(p.proc_root.join("self/kernel-release"), "7.0.0\n").unwrap();
            p.kthreadd(2_097_152);
            symlink("pid:[42]", p.proc_root.join("self/ns/pid")).unwrap();
            symlink("pid:[42]", p.proc_root.join("2/ns/pid")).unwrap();
            std::fs::write(
                p.proc_root.join("self/status"),
                "Name:\tt\nState:\tR (running)\nNSpid:\t7\nCapPrm:\t0000000000000000\n",
            )
            .unwrap();
            p.process(2, Path::new("/"), &[]);
            p
        }

        fn mountinfo(&self, canon: &Path, opts: &str) {
            std::fs::write(
                self.proc_root.join("self/mountinfo"),
                format!(
                    "1 23 0:55 / {} rw,nosuid,nodev,noexec,relatime - proc proc {opts}\n",
                    canon.display()
                ),
            )
            .unwrap();
        }

        fn kthreadd(&self, flags: u64) {
            std::fs::write(
                self.proc_root.join("2/stat"),
                format!("2 (kthreadd) S 0 0 0 0 -1 {flags} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n"),
            )
            .unwrap();
        }

        fn relink(&self, rel: &str, target: &str) {
            let path = self.proc_root.join(rel);
            let _ = std::fs::remove_file(&path);
            symlink(target, path).unwrap();
        }

        /// An own-uid, fully readable process.
        fn process(&self, pid: u32, cwd: &Path, fds: &[&Path]) -> PathBuf {
            let dir = self.proc_root.join(pid.to_string());
            std::fs::create_dir_all(dir.join("fd")).unwrap();
            let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
            std::fs::write(
                dir.join("status"),
                format!(
                    "Name:\tfixture\nState:\tS (sleeping)\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nGid:\t{gid}\t{gid}\t{gid}\t{gid}\nGroups:\t{gid}\nCapEff:\t0000000000000000\nCapPrm:\t0000000000000000\n"
                ),
            )
            .unwrap();
            symlink(cwd, dir.join("cwd")).unwrap();
            for (i, fd) in fds.iter().enumerate() {
                symlink(fd, dir.join("fd").join((3 + i).to_string())).unwrap();
            }
            dir
        }

        fn scan(&self) -> ProcessUse {
            process_use_under_from_proc_root(&self.lane, &self.proc_root).unwrap()
        }

        fn refused(&self, expect: &str) {
            let uses = self.scan();
            let err = uses.enumeration_error.expect("scan must be incomplete");
            assert!(err.contains(expect), "expected {expect:?} in {err:?}");
        }
    }

    #[test]
    fn complete_synthetic_view_with_harmless_process_is_clean() {
        let p = Proc::new();
        p.process(900, Path::new("/"), &[]);
        let uses = p.scan();
        assert!(
            uses.enumeration_error.is_none(),
            "{:?}",
            uses.enumeration_error
        );
        assert!(uses.cwd.is_empty() && uses.fd.is_empty());
    }

    #[test]
    fn own_uid_cwd_or_fd_inside_the_lane_is_named() {
        let p = Proc::new();
        p.process(901, &p.lane.canonicalize().unwrap(), &[]);
        p.process(902, Path::new("/"), &[&p.lane.canonicalize().unwrap()]);
        let uses = p.scan();
        assert_eq!(uses.cwd, vec![901]);
        assert_eq!(uses.fd, vec![902]);
    }

    #[test]
    fn own_uid_unexplained_permission_denied_is_an_incomplete_scan() {
        // Entry (cwd) is owned by the caller, so the kernel policy cannot
        // explain the EACCES on fd/: it must refuse, not excuse.
        let p = Proc::new();
        let dir = p.process(903, Path::new("/"), &[]);
        std::fs::set_permissions(dir.join("fd"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let uses = p.scan();
        std::fs::set_permissions(dir.join("fd"), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            uses.enumeration_error.is_some(),
            "unexplained EACCES on an own-uid process was excused"
        );
    }

    #[test]
    fn hidepid_hidden_view_is_refused() {
        let p = Proc::new();
        let canon = p.proc_root.canonicalize().unwrap();
        p.mountinfo(&canon, "rw,hidepid=2");
        p.refused("hidepid");
    }

    #[test]
    fn non_canonical_or_other_pid2_namespace_link_is_refused() {
        for bad in [
            "pid:[042]",
            "pid:[0]",
            "net:[42]",
            "pid:[43]",
            "garbage",
            "pid:[42]x",
        ] {
            let p = Proc::new();
            p.relink("2/ns/pid", bad);
            let uses = p.scan();
            assert!(
                uses.enumeration_error.is_some(),
                "PID 2 link {bad:?} was accepted"
            );
        }
    }

    #[test]
    fn caller_namespace_link_must_parse() {
        for bad in ["garbage", "pid:[0]", "pid:[042]"] {
            let p = Proc::new();
            p.relink("self/ns/pid", bad);
            assert!(p.scan().enumeration_error.is_some(), "caller link {bad:?}");
        }
    }

    #[test]
    fn pid2_must_be_kthreadd() {
        let p = Proc::new();
        p.kthreadd(0); // no PF_KTHREAD
        p.refused("kthreadd");
        let p = Proc::new();
        std::fs::write(
            p.proc_root.join("2/stat"),
            "2 (bash) S 0 0 0 0 -1 2097152 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        )
        .unwrap();
        p.refused("kthreadd");
    }

    #[test]
    fn nspid_proof_for_an_empty_pid2_link() {
        // The synthetic seam cannot make an empty symlink, so the empty-link
        // branch is driven through its two decisions directly.
        assert!(status_has_single_nspid("NSpid:\t7\n"));
        assert!(!status_has_single_nspid("NSpid:\t7\t1\n"));
        assert!(!status_has_single_nspid("NSpid:\t7\t3\t1\n"));
        assert!(!status_has_single_nspid(""));
    }
}

/// CAD-1209: a process the kernel hid from this caller and
/// `policy_denies_read` excused is named in the scan result, so finish can
/// report it instead of tolerating it silently.
#[cfg(all(test, target_os = "linux"))]
mod cad1209_excused_report {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn fixture(root: &Path) -> (PathBuf, PathBuf) {
        let (proc_root, lane) = (root.join("proc"), root.join("lane"));
        std::fs::create_dir_all(&lane).unwrap();
        std::fs::create_dir_all(proc_root.join("self/ns")).unwrap();
        std::fs::create_dir_all(proc_root.join("2/ns")).unwrap();
        let canon = proc_root.canonicalize().unwrap();
        std::fs::write(
            proc_root.join("self/mountinfo"),
            format!(
                "1 23 0:55 / {} rw,nosuid,nodev,noexec,relatime - proc proc rw\n",
                canon.display()
            ),
        )
        .unwrap();
        std::fs::write(proc_root.join("self/kernel-release"), "7.0.0\n").unwrap();
        std::fs::write(
            proc_root.join("2/stat"),
            "2 (kthreadd) S 0 0 0 0 -1 2097152 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        )
        .unwrap();
        symlink("pid:[42]", proc_root.join("self/ns/pid")).unwrap();
        symlink("pid:[42]", proc_root.join("2/ns/pid")).unwrap();
        std::fs::write(
            proc_root.join("self/status"),
            "Name:\tt\nState:\tR (running)\nNSpid:\t7\nCapPrm:\t0000000000000000\n",
        )
        .unwrap();
        process(&proc_root, 2, "0000000000000000");
        (proc_root, lane)
    }

    fn process(proc_root: &Path, pid: u32, cap_prm: &str) -> PathBuf {
        let dir = proc_root.join(pid.to_string());
        std::fs::create_dir_all(dir.join("fd")).unwrap();
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        std::fs::write(
            dir.join("status"),
            format!(
                "Name:\tfixture\nState:\tS (sleeping)\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nGid:\t{gid}\t{gid}\t{gid}\t{gid}\nGroups:\t{gid}\nCapEff:\t0000000000000000\nCapPrm:\t{cap_prm}\n"
            ),
        )
        .unwrap();
        symlink("/", dir.join("cwd")).unwrap();
        dir
    }

    #[test]
    fn a_policy_hidden_process_is_named_and_the_scan_stays_complete() {
        let root = tempfile::Builder::new()
            .prefix("c1209u-")
            .tempdir_in("/tmp")
            .unwrap();
        let (proc_root, lane) = fixture(root.path());
        // Capabilities the caller lacks make its fd/ directory unreadable
        // by policy; an ordinary process beside it is simply scanned.
        let hidden = process(&proc_root, 910, "000000000000ffff");
        process(&proc_root, 911, "0000000000000000");
        std::fs::set_permissions(hidden.join("fd"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let uses = process_use_under_from_proc_root(&lane, &proc_root).unwrap();
        std::fs::set_permissions(hidden.join("fd"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert!(
            uses.enumeration_error.is_none(),
            "{:?}",
            uses.enumeration_error
        );
        assert_eq!(uses.excused, vec![910]);
    }
}

/// CAD-1558 reviewer-written refusal acceptance check for the fd re-read
/// bound in `inspect_pid`, driven through `process_use_under_from_proc_root`
/// (the real guard). A fd link that stays unreadable while its entry is
/// present, or any other unexplained read error, keeps the scan incomplete;
/// a holder beside it is still named. The transient case (a link that
/// vanishes once and then resolves) needs a race between two syscalls and
/// cannot be produced deterministically from a synthetic tree.
#[cfg(all(test, target_os = "linux"))]
mod cad1558_acceptance {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::time::{Duration, Instant};

    fn fixture(root: &Path) -> (PathBuf, PathBuf) {
        let (proc_root, lane) = (root.join("proc"), root.join("lane"));
        std::fs::create_dir_all(&lane).unwrap();
        std::fs::create_dir_all(proc_root.join("self/ns")).unwrap();
        std::fs::create_dir_all(proc_root.join("2/ns")).unwrap();
        let canon = proc_root.canonicalize().unwrap();
        std::fs::write(
            proc_root.join("self/mountinfo"),
            format!(
                "1 23 0:55 / {} rw,nosuid,nodev,noexec,relatime - proc proc rw\n",
                canon.display()
            ),
        )
        .unwrap();
        std::fs::write(proc_root.join("self/kernel-release"), "7.0.0\n").unwrap();
        std::fs::write(
            proc_root.join("2/stat"),
            "2 (kthreadd) S 0 0 0 0 -1 2097152 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        )
        .unwrap();
        symlink("pid:[42]", proc_root.join("self/ns/pid")).unwrap();
        symlink("pid:[42]", proc_root.join("2/ns/pid")).unwrap();
        std::fs::write(
            proc_root.join("self/status"),
            "Name:\tt\nState:\tR (running)\nNSpid:\t7\nCapPrm:\t0000000000000000\n",
        )
        .unwrap();
        process(&proc_root, 2);
        (proc_root, lane)
    }

    /// An own-uid, live (`S`) process with an empty fd directory.
    fn process(proc_root: &Path, pid: u32) -> PathBuf {
        let dir = proc_root.join(pid.to_string());
        std::fs::create_dir_all(dir.join("fd")).unwrap();
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        std::fs::write(
            dir.join("status"),
            format!(
                "Name:\tfixture\nState:\tS (sleeping)\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nGid:\t{gid}\t{gid}\t{gid}\t{gid}\nGroups:\t{gid}\nCapEff:\t0000000000000000\nCapPrm:\t0000000000000000\n"
            ),
        )
        .unwrap();
        symlink("/", dir.join("cwd")).unwrap();
        dir
    }

    fn root() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("c1558u-")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[test]
    fn unreadable_fd_link_refuses_and_a_holder_is_still_named() {
        let root = root();
        let (proc_root, lane) = fixture(root.path());
        let holder = process(&proc_root, 921);
        symlink(lane.canonicalize().unwrap(), holder.join("fd/3")).unwrap();
        // A regular file where the fd link should be: readlink fails with
        // EINVAL, an error no policy explains.
        let bad = process(&proc_root, 920);
        std::fs::write(bad.join("fd/3"), "").unwrap();
        let uses = process_use_under_from_proc_root(&lane, &proc_root).unwrap();
        let err = uses.enumeration_error.expect("scan must be incomplete");
        assert!(
            err.contains("inspect file descriptor for process 920"),
            "{err}"
        );
        assert_eq!(uses.fd, vec![921]);

        std::fs::remove_dir_all(&bad).unwrap();
        let uses = process_use_under_from_proc_root(&lane, &proc_root).unwrap();
        assert!(
            uses.enumeration_error.is_none(),
            "{:?}",
            uses.enumeration_error
        );
        assert_eq!(uses.fd, vec![921]);
    }

    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// The kernel's own "readlink ENOENT, lstat Ok" pair, persistently:
    /// after `unshare(CLONE_NEWPID)` and before any child is forked,
    /// `/proc/<pid>/ns/pid_for_children` has no reaper to name. A fake
    /// pid's `fd` directory points at that real `ns` directory, so every
    /// bounded re-read sees exactly what a vanishing fd link gives.
    #[test]
    fn persistently_vanishing_fd_link_refuses_after_bounded_rereads() {
        let restricted =
            std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
                .map(|value| value.trim() == "1")
                .unwrap_or(false);
        if restricted {
            // Unprivileged CLONE_NEWPID is refused on this kernel (Ubuntu
            // 24.04 default); the case runs wherever the kernel allows it.
            eprintln!("cad1558: unprivileged pid namespaces restricted; case not run");
            return;
        }
        let child = Child(
            std::process::Command::new("/usr/bin/unshare")
                .args(["-U", "-r", "-p", "/bin/sleep", "60"])
                .stdin(std::process::Stdio::null())
                .spawn()
                .expect("spawn unshare for a reaperless pid namespace"),
        );
        let ns = PathBuf::from(format!("/proc/{}/ns", child.0.id()));
        let link = ns.join("pid_for_children");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let vanished = matches!(
                std::fs::read_link(&link),
                Err(ref e) if e.kind() == io::ErrorKind::NotFound
            ) && std::fs::symlink_metadata(&link).is_ok();
            if vanished {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "pid_for_children never became a vanishing link"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        let root = root();
        let (proc_root, lane) = fixture(root.path());
        let holder = process(&proc_root, 921);
        symlink(lane.canonicalize().unwrap(), holder.join("fd/3")).unwrap();
        let vanishing = process(&proc_root, 922);
        std::fs::remove_dir(vanishing.join("fd")).unwrap();
        symlink(&ns, vanishing.join("fd")).unwrap();
        let uses = process_use_under_from_proc_root(&lane, &proc_root).unwrap();
        let err = uses.enumeration_error.expect("scan must be incomplete");
        assert!(
            err.contains("inspect file descriptor for process 922")
                && err.contains("No such file or directory"),
            "{err}"
        );
        assert_eq!(uses.fd, vec![921]);
        drop(child);
    }
}
