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

/// CAD-1845 reviewer-written acceptance check (cc13-rev-spec) for the
/// EACCES answer on a namespace link. The real guard
/// (`ProcView::validate_completeness` through `read_namespace_link`) runs
/// unchanged; the only seam is the kernel's own seccomp user notification
/// on `readlinkat`: a supervisor thread answers the targeted link's read
/// with a chosen errno and lets every other read continue to the kernel.
/// The filter sits on one worker thread of a re-executed test process, so
/// it never reaches another test.
#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod cad1845_acceptance {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use tempfile::{Builder, TempDir};

    const ISOLATED: &str = "CADENCE_CAD1845_ACCEPT_ISOLATED";
    const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;

    struct Outcome {
        result: std::result::Result<(), String>,
        seen: Vec<String>,
        injected: Vec<String>,
    }

    /// Only `readlinkat` with the 64-byte buffer `read_namespace_link`
    /// uses is sent to the supervisor; everything else is allowed.
    fn install_listener() -> OwnedFd {
        let stmt = |code: u16, k: u32| libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k,
        };
        let jeq = |k: u32, jt: u8, jf: u8| libc::sock_filter {
            code: 0x05 | 0x10, // BPF_JMP | BPF_JEQ | BPF_K
            jt,
            jf,
            k,
        };
        let load = 0x20; // BPF_LD | BPF_W | BPF_ABS
        let ret = 0x06; // BPF_RET | BPF_K
        let mut filter = [
            stmt(load, 4), // seccomp_data.arch
            jeq(AUDIT_ARCH_X86_64, 0, 4),
            stmt(load, 0), // seccomp_data.nr
            jeq(libc::SYS_readlinkat as u32, 0, 2),
            stmt(load, 40), // low word of args[3] (bufsiz)
            jeq(64, 1, 0),
            stmt(ret, libc::SECCOMP_RET_ALLOW),
            stmt(ret, libc::SECCOMP_RET_USER_NOTIF),
        ];
        let prog = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_mut_ptr(),
        };
        unsafe {
            assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
            let fd = libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                libc::SECCOMP_FILTER_FLAG_NEW_LISTENER,
                &prog as *const libc::sock_fprog,
            );
            assert!(fd >= 0, "seccomp listener: {}", io::Error::last_os_error());
            OwnedFd::from_raw_fd(fd as i32)
        }
    }

    fn supervise(
        listener: &OwnedFd,
        target: Option<&str>,
        errno: i32,
        stop: &AtomicBool,
        seen: &Mutex<Vec<String>>,
        injected: &Mutex<Vec<String>>,
    ) {
        loop {
            let mut pfd = libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut pfd, 1, 50) };
            if ready == 0 {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                continue;
            }
            if ready < 0 || pfd.revents & libc::POLLIN == 0 {
                return;
            }
            let mut req: libc::seccomp_notif = unsafe { std::mem::zeroed() };
            if unsafe {
                libc::ioctl(
                    listener.as_raw_fd(),
                    libc::SECCOMP_IOCTL_NOTIF_RECV,
                    &mut req,
                )
            } != 0
            {
                continue;
            }
            let path =
                std::fs::read_link(format!("/proc/{}/fd/{}", req.pid, req.data.args[0] as i32))
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
            seen.lock().unwrap().push(path.clone());
            let mut resp: libc::seccomp_notif_resp = unsafe { std::mem::zeroed() };
            resp.id = req.id;
            if target.is_some_and(|suffix| path.ends_with(suffix)) {
                resp.error = -errno;
                injected.lock().unwrap().push(path);
            } else {
                resp.flags = libc::SECCOMP_USER_NOTIF_FLAG_CONTINUE as u32;
            }
            let sent =
                unsafe { libc::ioctl(listener.as_raw_fd(), libc::SECCOMP_IOCTL_NOTIF_SEND, &resp) };
            assert_eq!(sent, 0, "notif send: {}", io::Error::last_os_error());
        }
    }

    /// Runs the real completeness proof over `root`, answering the read of
    /// the link whose held path ends with `target` with `errno`.
    fn validate(root: &Path, target: Option<String>, errno: i32) -> Outcome {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let injected = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<OwnedFd>();
        let supervisor = {
            let (seen, injected, stop) = (seen.clone(), injected.clone(), stop.clone());
            std::thread::spawn(move || {
                if let Ok(listener) = rx.recv() {
                    supervise(&listener, target.as_deref(), errno, &stop, &seen, &injected);
                }
            })
        };
        let root = root.to_path_buf();
        let result = std::thread::spawn(move || {
            let view = ProcView::open(&root).map_err(|error| error.to_string())?;
            tx.send(install_listener()).unwrap();
            view.validate_completeness()
                .map_err(|error| error.to_string())
        })
        .join()
        .unwrap();
        stop.store(true, Ordering::SeqCst);
        supervisor.join().unwrap();
        let seen = seen.lock().unwrap().clone();
        let injected = injected.lock().unwrap().clone();
        Outcome {
            result,
            seen,
            injected,
        }
    }

    /// A synthetic view that completes cleanly; every link in it is owned
    /// by the caller.
    fn synthetic() -> (TempDir, PathBuf) {
        let root = Builder::new().prefix("c1845u-").tempdir_in("/tmp").unwrap();
        let proc_root = root.path().join("proc");
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
        (root, proc_root)
    }

    fn refused(outcome: &Outcome, case: &str, expect: &[&str]) {
        assert!(
            !outcome.injected.is_empty(),
            "{case}: the targeted link was never read (seen {:?})",
            outcome.seen
        );
        let error = outcome
            .result
            .as_ref()
            .err()
            .unwrap_or_else(|| panic!("{case}: accepted; seen {:?}", outcome.seen));
        for part in expect {
            assert!(
                error.contains(part),
                "{case}: expected {part:?} in {error:?}"
            );
        }
    }

    fn link_owner(path: &str) -> Option<u32> {
        std::fs::symlink_metadata(path).ok().map(|meta| meta.uid())
    }

    #[test]
    fn cad1845_namespace_link_refusals() {
        if std::env::var_os(ISOLATED).is_none() {
            let name = module_path!().split_once("::").unwrap().1.to_string()
                + "::cad1845_namespace_link_refusals";
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--test-threads", "1", "--nocapture"])
                .env(ISOLATED, "1")
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
        let euid = unsafe { libc::geteuid() };
        let pid2 = Some("/2/ns/pid".to_string());

        // 0. The seam itself changes nothing: an uninjected synthetic view
        // completes and both namespace links pass through the supervisor.
        let (_keep, root) = synthetic();
        let base = validate(&root, None, 0);
        assert!(base.result.is_ok(), "{:?}", base.result);
        assert!(
            base.seen.iter().any(|p| p.ends_with("/self/ns/pid"))
                && base.seen.iter().any(|p| p.ends_with("/2/ns/pid")),
            "supervisor did not see both links: {:?}",
            base.seen
        );

        // 1. EACCES on a PID 2 link the caller owns is not the CAD-1196
        // answer: it refuses.
        assert_eq!(
            link_owner(root.join("2/ns/pid").to_str().unwrap()),
            Some(euid)
        );
        let own = validate(&root, pid2.clone(), libc::EACCES);
        refused(
            &own,
            "own-uid EACCES on 2/ns/pid",
            &["2/ns/pid", "Permission denied"],
        );

        // 2. The caller's own link unreadable refuses.
        let me = validate(&root, Some("/self/ns/pid".into()), libc::EACCES);
        refused(&me, "EACCES on self/ns/pid", &["self/ns/pid"]);

        // 3. Any other errno on PID 2's link refuses.
        for errno in [libc::EIO, libc::EPERM, libc::ENOENT] {
            let other = validate(&root, pid2.clone(), errno);
            refused(&other, &format!("errno {errno} on 2/ns/pid"), &["2/ns/pid"]);
        }

        // 4-6. The real /proc, where PID 2's link is root's (foreign to a
        // non-root caller). Driven wherever the uninjected proof completes;
        // a container whose PID 2 is not kthreadd cannot reach the link.
        let real = Path::new("/proc");
        let base = validate(real, None, 0);
        let foreign = link_owner("/proc/2/ns/pid").is_some_and(|uid| uid != euid);
        if let Err(reason) = &base.result {
            eprintln!(
                "cad1845: real /proc proof does not complete here ({reason}); cases 4-6 not run"
            );
            return;
        }
        // 4. A foreign link answering EACCES is the accepted residual: the
        // proof completes through the kthreadd + NSpid path.
        if foreign {
            let excused = validate(real, pid2.clone(), libc::EACCES);
            assert!(!excused.injected.is_empty(), "{:?}", excused.seen);
            assert!(excused.result.is_ok(), "{:?}", excused.result);
        }
        // 5. A foreign link answering any other errno still refuses.
        for errno in [libc::EIO, libc::EPERM] {
            let other = validate(real, pid2.clone(), errno);
            refused(
                &other,
                &format!("real errno {errno} on 2/ns/pid"),
                &["2/ns/pid"],
            );
        }
        // 6. The caller's own link made foreign (non-dumpable: the kernel
        // then reports root as its owner) and unreadable still refuses: an
        // excused None for the caller proves nothing.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) }, 0);
        let self_link = format!("/proc/{}/ns/pid", std::process::id());
        let self_foreign = link_owner(&self_link).is_some_and(|uid| uid != euid);
        let me = validate(
            real,
            Some(format!("/{}/ns/pid", std::process::id())),
            libc::EACCES,
        );
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0) }, 0);
        // Root owns every link, so there the own-uid refusal answers instead.
        assert!(
            self_foreign || euid == 0,
            "non-dumpable self link still owned by caller"
        );
        let expect = if self_foreign {
            "own pid namespace"
        } else {
            "self/ns/pid"
        };
        refused(&me, "EACCES on a foreign self/ns/pid", &[expect]);
    }
}
